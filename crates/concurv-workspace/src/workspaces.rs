//! `concurv workspace add` and `concurv workspace list` (RFC 0004 §3).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use concurv_core::{ConcurvError, ErrorCode, RandomId, Result, SnapshotId, WorkspaceId};
use concurv_objects::encoding::validate_workspace_name;
use concurv_objects::fs::{create_dir_durable, rename_durable, sync_dir, write_file_atomic};
use concurv_objects::{Operation, WorkspaceView};

use crate::repo::{Repo, RepoOptions, WORKSPACE_FORMAT, WORKSPACE_TMP, simplify};
use crate::restore::{Checkout, checkout};

/// A workspace as listed by `workspace list`.
#[derive(Clone, Debug)]
pub struct WorkspaceInfo {
    /// Id.
    pub id: WorkspaceId,
    /// Name.
    pub name: String,
    /// Branch it publishes to.
    pub branch: Option<String>,
    /// Head snapshot.
    pub head: Option<SnapshotId>,
    /// Root directory, if known on this device.
    pub root: Option<PathBuf>,
    /// The workspace created by `init`.
    pub primary: bool,
    /// The workspace this command runs in.
    pub current: bool,
}

/// Result of `workspace add`.
#[derive(Debug)]
pub struct AddOutcome {
    /// The new workspace.
    pub info: WorkspaceInfo,
    /// Files written while checking out the branch head.
    pub written: usize,
}

fn primary_root(repo: &Repo) -> Option<PathBuf> {
    repo.store_dir().parent().map(Path::to_path_buf)
}

fn locator(repo: &Repo, id: &WorkspaceId) -> Option<PathBuf> {
    fs::read_to_string(repo.locator_path(id))
        .ok()
        .map(|s| PathBuf::from(s.trim_end_matches('\n')))
}

/// All workspaces in the current view.
pub fn list(repo: &Repo) -> Result<Vec<WorkspaceInfo>> {
    let (_, op, _) = repo.current()?;
    let primary_id = primary_id(repo)?;
    let mut out: Vec<WorkspaceInfo> = op
        .view
        .workspaces
        .iter()
        .map(|(id, w)| {
            let primary = Some(*id) == primary_id;
            WorkspaceInfo {
                id: *id,
                name: w.name.clone(),
                branch: w.branch.clone(),
                head: w.head,
                root: if primary {
                    primary_root(repo)
                } else {
                    locator(repo, id)
                },
                primary,
                current: *id == repo.workspace_id(),
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn primary_id(repo: &Repo) -> Result<Option<WorkspaceId>> {
    let p = repo.store_dir().join("workspace_id");
    let s = fs::read_to_string(&p).map_err(|e| ConcurvError::io(e, "read", &p))?;
    Ok(WorkspaceId::from_hex(s.trim_end_matches('\n')).ok())
}

/// Canonicalize `p`; `p` must exist.
fn canon(p: &Path) -> Result<PathBuf> {
    fs::canonicalize(p)
        .map(simplify)
        .map_err(|e| ConcurvError::io(e, "resolve", p))
}

/// Create a new workspace at `path` on the same branch, and check out the
/// branch head there.
pub fn add(repo: &Repo, path: &Path, name: Option<&str>) -> Result<AddOutcome> {
    add_on(repo, path, name, None)
}

/// [`add`], tracking `branch` instead of the current workspace's branch
/// (ADR 0007). The branch must exist.
pub fn add_on(
    repo: &Repo,
    path: &Path,
    name: Option<&str>,
    branch: Option<&str>,
) -> Result<AddOutcome> {
    if let Some(b) = branch
        && !repo.current()?.1.view.branches.contains_key(b)
    {
        return Err(ConcurvError::new(
            ErrorCode::NotFound,
            format!("no branch named `{b}` (see `concurv branch list`); nothing was created"),
        ));
    }
    // Target directory: absent (created) or an empty directory.
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {
            let empty = fs::read_dir(path)
                .map_err(|e| ConcurvError::io(e, "read directory", path))?
                .next()
                .is_none();
            if !empty {
                return Err(ConcurvError::new(
                    ErrorCode::InvalidArgument,
                    format!(
                        "`{}` is not empty; a new workspace needs an empty or new directory",
                        path.display()
                    ),
                ));
            }
        }
        Ok(_) => {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!("`{}` exists and is not a directory", path.display()),
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|e| ConcurvError::io(e, "create workspace directory", path))?;
        }
        Err(e) => return Err(ConcurvError::io(e, "inspect", path)),
    }
    let root = canon(path)?;
    let name = match name {
        Some(n) => n.to_owned(),
        None => root
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                ConcurvError::new(
                    ErrorCode::InvalidArgument,
                    "cannot derive a workspace name; pass --name",
                )
            })?,
    };
    validate_workspace_name(&name)?;

    // A workspace inside another one (or containing one) would be versioned
    // by the outer workspace's scan.
    for other in list(repo)? {
        let Some(r) = other.root.as_deref().and_then(|r| canon(r).ok()) else {
            continue;
        };
        if root.starts_with(&r) || r.starts_with(&root) {
            return Err(ConcurvError::new(
                ErrorCode::InvalidPath,
                format!(
                    "`{}` overlaps workspace `{}` at `{}`; workspaces must not be nested",
                    root.display(),
                    other.name,
                    r.display()
                ),
            ));
        }
    }

    // Metadata directory, built in staging and renamed into place.
    let faults = &repo.options().faults;
    let id = WorkspaceId::generate()?;
    let staging = root.join(format!(".concurv-init-{}", RandomId::generate()?.to_hex()));
    create_dir_durable(&staging, faults)?;
    let tmp = staging.join(WORKSPACE_TMP);
    create_dir_durable(&tmp, faults)?;
    let store_dir = canon(repo.store_dir())?;
    let store_str = store_dir.to_str().ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::InvalidPath,
            "the repository path is not valid UTF-8",
        )
    })?;
    write_file_atomic(
        &tmp,
        &staging.join("format"),
        WORKSPACE_FORMAT.as_bytes(),
        faults,
    )?;
    write_file_atomic(
        &tmp,
        &staging.join("store"),
        format!("{store_str}\n").as_bytes(),
        faults,
    )?;
    write_file_atomic(
        &tmp,
        &staging.join("workspace_id"),
        format!("{}\n", id.to_hex()).as_bytes(),
        faults,
    )?;
    sync_dir(&staging, faults)?;
    rename_durable(&staging, &root.join(".concurv"), faults)?;

    // Register: locator file + operation.
    let root_str = root.to_str().ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::InvalidPath,
            "the workspace path is not valid UTF-8",
        )
    })?;
    let requested = branch;
    let branch;
    let main_head;
    {
        let lock = repo.lock()?;
        let (op_id, op, current) = repo.current()?;
        if op.view.workspaces.values().any(|w| w.name == name) {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!("a workspace named `{name}` already exists"),
            ));
        }
        branch = requested
            .map(str::to_owned)
            .or_else(|| current.branch.clone());
        main_head = branch
            .as_ref()
            .and_then(|b| op.view.branches.get(b).copied());
        let locators = repo.store_dir().join("workspaces");
        create_dir_durable(&locators, faults)?;
        write_file_atomic(
            repo.store().tmp_dir(),
            &repo.locator_path(&id),
            format!("{root_str}\n").as_bytes(),
            faults,
        )?;
        let mut view = op.view.clone();
        view.workspaces.insert(
            id,
            WorkspaceView {
                name: name.clone(),
                branch: branch.clone(),
                head: None,
                merge: None,
            },
        );
        let new_op = Operation {
            parents: vec![op_id],
            timestamp_ms: repo.now_ms(),
            device_id: repo.device_id(),
            workspace_id: Some(id),
            kind: "workspace-add".into(),
            description: format!("add workspace {name}"),
            backup: None,
            undoes: None,
            checked_out: None,
            view,
        };
        repo.oplog().commit(&lock, &op_id, &new_op)?;
    }

    // Check out the branch head in the new workspace.
    let ws_repo = Repo::open(
        &root,
        RepoOptions {
            ..repo.options().clone()
        },
    )?;
    let mut written = 0;
    let mut head = None;
    if let (Some(m), Some(b)) = (main_head, branch.as_ref()) {
        let ws_lock = ws_repo.workspace_lock()?;
        let out = checkout(
            &ws_repo,
            &ws_lock,
            m,
            Checkout {
                force: false,
                set_head: true,
                kind: "update",
                description: format!("check out {b} {m} into new workspace {name}"),
                expected: None,
                undoes: None,
                set_merge: None,
                set_branch: None,
                transactional: false,
            },
        )?;
        written = out.written;
        head = Some(m);
    }
    Ok(AddOutcome {
        info: WorkspaceInfo {
            id,
            name,
            branch,
            head,
            root: Some(root),
            primary: false,
            current: false,
        },
        written,
    })
}

/// Result of `workspace remove`.
#[derive(Debug)]
pub struct RemoveOutcome {
    /// The workspace as it was.
    pub info: WorkspaceInfo,
    /// Whether its directory was found (and checked for unsaved changes).
    pub directory_checked: bool,
    /// Operation recorded.
    pub operation: concurv_core::OperationId,
}

/// Unregister workspace `name` (backlog P1-8). Its directory and files stay
/// on disk untouched; only the repository forgets the workspace. Refused,
/// with nothing changed: the primary workspace or the one this command runs
/// in (`CONCURV_INVALID_ARGUMENT`); a pending merge there (`CONCURV_CONFLICT`);
/// saves in it that no branch, tag, other workspace or remote-tracking
/// reference contains (`CONCURV_DIVERGED`: publish them, or keep them with
/// `branch create`/`tag create --at <head>`); unsaved changes in its
/// directory, when that directory still exists (`CONCURV_DIRTY_WORKSPACE`).
/// The workspace lock of the removed workspace is held throughout, so no
/// command runs there meanwhile.
pub fn remove(repo: &Repo, name: &str) -> Result<RemoveOutcome> {
    let info = list(repo)?
        .into_iter()
        .find(|w| w.name == name)
        .ok_or_else(|| {
            ConcurvError::new(
                ErrorCode::NotFound,
                format!("no workspace named `{name}` (see `concurv workspace list`)"),
            )
        })?;
    if info.primary {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            "the primary workspace holds the repository and cannot be removed",
        ));
    }
    if info.current {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            "a workspace cannot remove itself: run `concurv workspace remove` from another workspace",
        ));
    }
    // Lock the workspace being removed, if its directory is still there and
    // still belongs to it; then check its files.
    let other = info.root.as_ref().and_then(|root| {
        Repo::open(root, repo.options().clone())
            .ok()
            .filter(|o| o.workspace_id() == info.id)
    });
    let _other_lock = match &other {
        Some(o) => Some(o.workspace_lock()?),
        None => None,
    };
    if let Some(o) = &other {
        let st = crate::status::status(o)?;
        if !st.changes.is_empty() || st.interrupted_restore.is_some() {
            return Err(ConcurvError::new(
                ErrorCode::DirtyWorkspace,
                format!(
                    "workspace `{name}` has {} unsaved change(s){} in `{}`; save or restore them \
                     first. Nothing was changed",
                    st.changes.len(),
                    if st.interrupted_restore.is_some() {
                        " and an interrupted restore"
                    } else {
                        ""
                    },
                    o.root().display()
                ),
            ));
        }
    }
    let lock = repo.lock()?;
    let (op_id, op, _) = repo.current()?;
    let Some(ws) = op.view.workspaces.get(&info.id).cloned() else {
        return Err(ConcurvError::new(
            ErrorCode::StaleHead,
            format!("workspace `{name}` was removed meanwhile; nothing was changed"),
        ));
    };
    if ws.merge.is_some() {
        return Err(ConcurvError::new(
            ErrorCode::Conflict,
            format!(
                "a merge is pending in workspace `{name}`: finish it (`concurv save`) or give it up \
                 (`concurv update --abort`) there first. Nothing was changed"
            ),
        ));
    }
    if let Some(h) = ws.head
        && !crate::branches::kept_elsewhere(
            repo,
            &op.view,
            crate::branches::Except::Workspace(info.id),
            h,
        )?
    {
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "workspace `{name}` has saves that no branch, tag, other workspace or \
                 remote-tracking reference contains; publish them there, or keep them with \
                 `concurv tag create <tag> --at {h}`. Nothing was changed"
            ),
        ));
    }
    let mut view = op.view.clone();
    view.workspaces.remove(&info.id);
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "workspace-remove".into(),
        description: format!(
            "remove workspace {name}{}",
            info.root
                .as_ref()
                .map(|r| format!(" ({})", r.display()))
                .unwrap_or_default()
        ),
        backup: None,
        undoes: None,
        checked_out: None,
        view,
    };
    let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
    // The locator is a cache of the directory's path; forget it.
    let _ = fs::remove_file(repo.locator_path(&info.id));
    Ok(RemoveOutcome {
        directory_checked: other.is_some(),
        info,
        operation,
    })
}
