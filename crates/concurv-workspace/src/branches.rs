//! Named branches (R2.5, ADR 0007: `docs/adr/0007-branch-model.md`).
//!
//! A branch is a shared, named reference to a snapshot in the operation
//! view. It moves only forward, by compare-and-swap (`publish`). Every
//! workspace tracks exactly one branch, its `publish` and `update` target.
//!
//! Invariant B1 (reachability): no branch command makes a snapshot that was
//! a workspace head or a branch head unreachable from the current workspace
//! heads and branch heads. `create` only adds a reference; `switch` refuses
//! if the workspace head would become reachable from nothing.
//!
//! Deleting and renaming branches are not implemented (roadmap).

use concurv_core::{ConcurvError, ErrorCode, OperationId, Result, SnapshotId};
use concurv_objects::Operation;
use concurv_objects::encoding::validate_branch_name;

use crate::graph::is_ancestor;
use crate::refname;
use crate::repo::Repo;
use crate::restore::{Checkout, checkout, clear_marker, read_marker};

/// A branch as listed by `branch list`.
#[derive(Clone, Debug)]
pub struct BranchInfo {
    /// Name.
    pub name: String,
    /// Head snapshot (the local branch reference).
    pub head: SnapshotId,
    /// Names of the workspaces tracking it, sorted.
    pub workspaces: Vec<String>,
    /// The current workspace tracks it.
    pub current: bool,
}

/// Options for [`create`].
#[derive(Clone, Copy, Debug, Default)]
pub struct CreateOptions {
    /// Start point; default: this workspace's head (including saves not yet
    /// published), else the head of the branch it tracks.
    pub at: Option<SnapshotId>,
    /// Also make this workspace track the new branch, in the same operation.
    /// Only allowed at this workspace's head, so no file changes.
    pub switch: bool,
}

/// Result of `branch create`.
#[derive(Debug)]
pub struct CreateOutcome {
    /// New branch.
    pub name: String,
    /// Its head.
    pub head: SnapshotId,
    /// The workspace now tracks it (`--switch`).
    pub switched: bool,
    /// Operation recorded.
    pub operation: OperationId,
}

/// Result of `branch switch`.
#[derive(Debug)]
pub struct SwitchOutcome {
    /// Branch tracked before.
    pub from: Option<String>,
    /// Branch tracked now.
    pub to: String,
    /// Workspace head after the switch (the branch head).
    pub head: SnapshotId,
    /// Files written.
    pub written: usize,
    /// Files deleted.
    pub deleted: usize,
    /// True if this finished a switch that had been interrupted.
    pub resumed: bool,
    /// Operation recorded, or `None` if already on the branch.
    pub operation: Option<OperationId>,
}

/// All rules for a new branch name, including collisions with `existing`.
pub fn validate_new_branch_name<'a>(
    name: &str,
    existing: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    validate_branch_name(name)?;
    let existing: Vec<&String> = existing.into_iter().collect();
    refname::check_new(name, &existing)
}

/// All branches, sorted by name.
pub fn list(repo: &Repo) -> Result<Vec<BranchInfo>> {
    let (_, op, ws) = repo.current()?;
    Ok(op
        .view
        .branches
        .iter()
        .map(|(name, head)| {
            let mut workspaces: Vec<String> = op
                .view
                .workspaces
                .values()
                .filter(|w| w.branch.as_deref() == Some(name))
                .map(|w| w.name.clone())
                .collect();
            workspaces.sort();
            BranchInfo {
                name: name.clone(),
                head: *head,
                workspaces,
                current: ws.branch.as_deref() == Some(name),
            }
        })
        .collect())
}

/// Create branch `name` (ADR 0007 §2). By default it starts at this
/// workspace's head, so saves not yet published to the tracked branch are
/// on the new branch. Moves no other workspace and no other branch.
pub fn create(repo: &Repo, name: &str, opts: CreateOptions) -> Result<CreateOutcome> {
    // The workspace lock serializes `--switch` with other commands in this
    // workspace (its head and branch cannot change underneath).
    let _ws_lock = if opts.switch {
        Some(repo.workspace_lock()?)
    } else {
        None
    };
    let (_, op, ws) = repo.current()?;
    validate_new_branch_name(name, op.view.branches.keys())?;
    let default_at = ws.head.or_else(|| {
        ws.branch
            .as_ref()
            .and_then(|b| op.view.branches.get(b).copied())
    });
    let head = match opts.at.or(default_at) {
        Some(h) => h,
        None => {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                "nothing to branch from: save a snapshot first, or pass --at",
            ));
        }
    };
    if opts.switch {
        if ws.merge.is_some() {
            return Err(ConcurvError::new(
                ErrorCode::Conflict,
                "a merge is pending in this workspace: finish it with `concurv save`, or \
                 `concurv update --abort`, before switching branches",
            ));
        }
        if ws.head != Some(head) {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                "--switch creates the branch at this workspace's head (no files change); \
                 to start elsewhere, create the branch and use `concurv branch switch`",
            ));
        }
    }
    // The reference may only name a complete snapshot: read the snapshot and
    // every tree under it (blobs are written before the trees naming them).
    repo.snapshot_files(&head)?;
    let lock = repo.lock()?;
    let (op_id, op, current_ws) = repo.current()?;
    // Re-check under the repository lock: the compare-and-swap that decides
    // concurrent creations.
    validate_new_branch_name(name, op.view.branches.keys())?;
    if opts.switch && (current_ws.head != ws.head || current_ws.branch != ws.branch) {
        return Err(ConcurvError::new(
            ErrorCode::StaleHead,
            "this workspace changed while creating the branch; nothing was recorded, retry",
        ));
    }
    let mut view = op.view.clone();
    view.branches.insert(name.to_owned(), head);
    if opts.switch
        && let Some(w) = view.workspaces.get_mut(&repo.workspace_id())
    {
        w.branch = Some(name.to_owned());
    }
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "branch-create".into(),
        description: if opts.switch {
            format!(
                "create branch {name} at {head} and switch workspace {} to it",
                ws.name
            )
        } else {
            format!("create branch {name} at {head}")
        },
        backup: None,
        undoes: None,
        checked_out: None,
        view,
    };
    let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
    Ok(CreateOutcome {
        name: name.to_owned(),
        head,
        switched: opts.switch,
        operation,
    })
}

/// Make this workspace track `name` and check out that branch's head,
/// transactionally (ADR 0007 §3).
///
/// Refused, with nothing changed, if: the working copy has unsaved changes
/// to tracked files, or untracked or ignored files would be overwritten
/// (`CONCURV_DIRTY_WORKSPACE`); a merge is pending (`CONCURV_CONFLICT`); the
/// branch does not exist (`CONCURV_NOT_FOUND`); or the workspace head holds
/// saves that neither the old nor the new branch contains
/// (`CONCURV_DIVERGED`, invariant B1). Untracked files that do not collide
/// stay in place. If writing files or committing fails, the working copy is
/// rolled back. If the process dies midway, running the same switch again
/// finishes it (every file then holds its old or its new content).
pub fn switch(repo: &Repo, name: &str) -> Result<SwitchOutcome> {
    let ws_lock = repo.workspace_lock()?;
    let (_, op, ws) = repo.current()?;
    let Some(target) = op.view.branches.get(name).copied() else {
        return Err(ConcurvError::new(
            ErrorCode::NotFound,
            format!("no branch named `{name}` (see `concurv branch list`)"),
        ));
    };
    if ws.merge.is_some() {
        return Err(ConcurvError::new(
            ErrorCode::Conflict,
            "a merge is pending in this workspace: finish it with `concurv save`, or \
             `concurv update --abort`, before switching branches",
        ));
    }
    let marker = read_marker(repo)?;
    let from = ws.branch.clone();
    if from.as_deref() == Some(name) {
        // A switch whose commit succeeded but whose marker removal failed.
        if let Some(m) = &marker
            && m.branch.as_deref() == Some(name)
            && ws.head == Some(m.target)
        {
            clear_marker(repo, &ws_lock)?;
        }
        return Ok(SwitchOutcome {
            from,
            to: name.to_owned(),
            head: ws.head.unwrap_or(target),
            written: 0,
            deleted: 0,
            resumed: false,
            operation: None,
        });
    }
    // Resuming an interrupted switch to this branch: finish it at the target
    // it was started with, even if the branch has moved since.
    let resumed = marker
        .as_ref()
        .filter(|m| m.branch.as_deref() == Some(name) && m.origin.is_some() && m.origin == ws.head);
    let target = resumed.map_or(target, |m| m.target);
    if let Some(h) = ws.head {
        let old = from.as_ref().and_then(|b| op.view.branches.get(b).copied());
        let kept = match old {
            Some(m) => is_ancestor(repo.store(), h, m)?,
            None => false,
        } || is_ancestor(repo.store(), h, target)?;
        if !kept {
            let old_name = from.as_deref().unwrap_or("(none)");
            return Err(ConcurvError::new(
                ErrorCode::Diverged,
                format!(
                    "this workspace has saved work that is not on `{old_name}` or `{name}`; switching \
                     would leave it on no branch, so nothing was changed. Publish it first, or keep \
                     it on a new branch: `concurv branch create <new> --switch`"
                ),
            ));
        }
    }
    let resumed = resumed.is_some();
    let out = checkout(
        repo,
        &ws_lock,
        target,
        Checkout {
            force: false,
            set_head: true,
            kind: "branch-switch",
            description: format!(
                "switch workspace {} from {} to {name} at {target}",
                ws.name,
                from.as_deref().unwrap_or("(none)")
            ),
            expected: None,
            undoes: None,
            set_merge: None,
            set_branch: Some(Some(name.to_owned())),
            transactional: true,
        },
    )?;
    Ok(SwitchOutcome {
        from,
        to: name.to_owned(),
        head: target,
        written: out.written,
        deleted: out.deleted,
        resumed,
        operation: Some(out.operation),
    })
}
