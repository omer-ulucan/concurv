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
        branch = current.branch.clone();
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
