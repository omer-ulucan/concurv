//! Named branches (R2.5, ADR 0007: `docs/adr/0007-branch-model.md`).
//!
//! A branch is a shared, named pointer to a snapshot in the operation view;
//! it moves only forward, by compare-and-swap (`publish`). Every workspace
//! tracks exactly one branch, its publish and update target. `create` adds
//! a branch, `list` shows them, `switch` points the current workspace at
//! another branch and checks out its head. Deleting and renaming branches
//! are not implemented.

use concurv_core::{ConcurvError, ErrorCode, OperationId, Result, SnapshotId};
use concurv_objects::Operation;
use concurv_objects::encoding::validate_branch_name;

use crate::graph::is_ancestor;
use crate::repo::Repo;
use crate::restore::{Checkout, checkout};

/// A branch as listed by `branch list`.
#[derive(Clone, Debug)]
pub struct BranchInfo {
    /// Name.
    pub name: String,
    /// Head snapshot.
    pub head: SnapshotId,
    /// Names of the workspaces tracking it, sorted.
    pub workspaces: Vec<String>,
    /// The current workspace tracks it.
    pub current: bool,
}

/// Result of `branch create`.
#[derive(Debug)]
pub struct CreateOutcome {
    /// New branch.
    pub name: String,
    /// Its head.
    pub head: SnapshotId,
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
    /// Operation recorded, or `None` if already on the branch.
    pub operation: Option<OperationId>,
}

/// Rules for **new** branch names: a subset of Git's ref-name rules, so
/// every branch can be exported to Git under the same name. Names already
/// in a view are only checked by the general format rule (RFC 0001).
pub fn validate_new_branch_name(name: &str) -> Result<()> {
    validate_branch_name(name)?;
    let bad = |why: &str| {
        Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!("invalid branch name {name:?}: {why}"),
        ))
    };
    if name.len() > 200 {
        return bad("longer than 200 bytes");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
    {
        return bad("use only ASCII letters, digits and `._-/`");
    }
    if name == "HEAD" || name.starts_with('-') {
        return bad("reserved");
    }
    if name.contains("..") || name.contains("//") || name.ends_with(".lock") {
        return bad("`..`, `//` and a `.lock` suffix are not allowed");
    }
    if name
        .split('/')
        .any(|c| c.is_empty() || c.starts_with('.') || c.ends_with('.'))
    {
        return bad("a `/`-separated part is empty or starts or ends with `.`");
    }
    Ok(())
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

/// Create branch `name` at `at`, by default at this workspace's head (or,
/// without one, at the head of the branch it tracks). Moves no workspace.
pub fn create(repo: &Repo, name: &str, at: Option<SnapshotId>) -> Result<CreateOutcome> {
    validate_new_branch_name(name)?;
    let (_, op, ws) = repo.current()?;
    let head = match at {
        Some(s) => s,
        None => ws
            .head
            .or_else(|| {
                ws.branch
                    .as_ref()
                    .and_then(|b| op.view.branches.get(b).copied())
            })
            .ok_or_else(|| {
                ConcurvError::new(
                    ErrorCode::InvalidArgument,
                    "nothing to branch from: save a snapshot first, or pass --at",
                )
            })?,
    };
    // The pointer may only name a complete snapshot: read the snapshot and
    // every tree under it (blobs are written before the trees naming them).
    repo.snapshot_files(&head)?;
    let lock = repo.lock()?;
    let (op_id, op, _) = repo.current()?;
    if op.view.branches.contains_key(name) {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!("a branch named `{name}` already exists; nothing was changed"),
        ));
    }
    let mut view = op.view.clone();
    view.branches.insert(name.to_owned(), head);
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "branch-create".into(),
        description: format!("create branch {name} at {head}"),
        backup: None,
        undoes: None,
        checked_out: None,
        view,
    };
    let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
    Ok(CreateOutcome {
        name: name.to_owned(),
        head,
        operation,
    })
}

/// Make this workspace track `name` and check out that branch's head.
///
/// Refused, with nothing changed, if the working copy has unsaved changes
/// (`CONCURV_DIRTY_WORKSPACE`), a merge is pending (`CONCURV_CONFLICT`), the
/// branch does not exist (`CONCURV_NOT_FOUND`), or the workspace head holds
/// saves that neither the old nor the new branch contains
/// (`CONCURV_DIVERGED`): switching would leave them on no branch.
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
    let from = ws.branch.clone();
    if from.as_deref() == Some(name) {
        return Ok(SwitchOutcome {
            from,
            to: name.to_owned(),
            head: ws.head.unwrap_or(target),
            written: 0,
            deleted: 0,
            operation: None,
        });
    }
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
                     it on a new branch: `concurv branch create <new>` then switch"
                ),
            ));
        }
    }
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
        },
    )?;
    Ok(SwitchOutcome {
        from,
        to: name.to_owned(),
        head: target,
        written: out.written,
        deleted: out.deleted,
        operation: Some(out.operation),
    })
}

#[cfg(test)]
mod tests {
    use super::validate_new_branch_name as v;

    #[test]
    fn new_branch_names() {
        for ok in ["main", "feature/login", "v1.2", "agent-7_fix", "a/b/c"] {
            assert!(v(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "HEAD", "-x", "a..b", "a//b", "x.lock", "/a", "a/", ".hidden", "a/.b", "a.",
            "sp ace", "ü", "a:b", "a~1", "a^", "a*", "a?", "a[", "a\\b",
        ] {
            assert!(v(bad).is_err(), "{bad:?}");
        }
    }
}
