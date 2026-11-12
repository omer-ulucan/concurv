//! `concurv undo`: workspace-scoped undo (RFC 0004 §10).
//!
//! Undo reverses the most recent operation of *this* workspace that has not
//! been undone yet, and exactly what that operation changed: the workspace
//! head, the working files, or both. It never changes another workspace's
//! state or any branch, so it cannot revert shared history.

use std::collections::HashSet;

use concurv_core::{ConcurvError, ErrorCode, OperationId, Result, SnapshotId};
use concurv_objects::Operation;

use crate::repo::Repo;
use crate::restore::{Checkout, checkout};

/// Result of an undo.
#[derive(Debug)]
pub struct UndoOutcome {
    /// The operation that was reversed.
    pub undone: OperationId,
    /// Its kind (`save`, `update`, `restore`).
    pub undone_kind: String,
    /// Its description.
    pub undone_description: String,
    /// Workspace head after the undo.
    pub head: Option<SnapshotId>,
    /// Snapshot the working files were set to, if files changed.
    pub files_restored_to: Option<SnapshotId>,
    /// Files written / deleted in the working copy.
    pub written: usize,
    /// Files deleted.
    pub deleted: usize,
    /// The `undo` operation recorded.
    pub operation: OperationId,
}

/// Find this workspace's most recent operation that is neither an undo nor
/// already undone.
fn undo_target(repo: &Repo) -> Result<(OperationId, Operation)> {
    let ws = repo.workspace_id();
    let mut undone: HashSet<OperationId> = HashSet::new();
    for (id, op) in repo.oplog().walk(usize::MAX)? {
        if op.workspace_id != Some(ws) {
            continue;
        }
        if op.kind == "undo" {
            undone.extend(op.undoes);
            continue;
        }
        if !undone.contains(&id) {
            return Ok((id, op));
        }
    }
    Err(ConcurvError::new(
        ErrorCode::NotFound,
        "nothing to undo in this workspace",
    ))
}

fn refuse(kind: &str, why: &str) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::InvalidArgument,
        format!(
            "the most recent operation of this workspace is `{kind}`, which cannot be undone: {why}. \
             Nothing was changed"
        ),
    )
}

/// Reverse the most recent not-yet-undone operation of this workspace.
pub fn undo(repo: &Repo) -> Result<UndoOutcome> {
    let ws_lock = repo.workspace_lock()?;
    let ws_id = repo.workspace_id();
    let (target_id, target) = undo_target(repo)?;
    match target.kind.as_str() {
        "save" | "update" | "merge" | "restore" | "branch-switch" => {}
        "branch-create" => {
            return Err(refuse(
                "branch-create",
                "branches are shared and others may already use it; deleting branches is not \n                 supported yet",
            ));
        }
        kind @ ("tag-create" | "tag-delete") => {
            return Err(refuse(
                kind,
                "tags are shared and others may already rely on them; use `concurv tag delete` \
                 or `concurv tag create <name> --at <snapshot>` instead",
            ));
        }
        "publish" => {
            return Err(refuse(
                "publish",
                "it changed the shared branch, which other workspaces may already build on; \
                 make a new change that reverts it instead",
            ));
        }
        "workspace-add" => {
            return Err(refuse(
                "workspace-add",
                "removing workspaces is not supported yet",
            ));
        }
        other => return Err(refuse(other, "this operation kind has no undo")),
    }
    let parent_id = *target
        .parents
        .first()
        .ok_or_else(|| ConcurvError::internal("non-root operation without a parent"))?;
    let parent = repo.store().read_operation(&parent_id)?;
    let before = parent
        .view
        .workspaces
        .get(&ws_id)
        .cloned()
        .ok_or_else(|| refuse(&target.kind, "the workspace did not exist before it"))?;
    let after = target
        .view
        .workspaces
        .get(&ws_id)
        .cloned()
        .ok_or_else(|| ConcurvError::internal("undo target lacks this workspace"))?;
    let (_, _, current) = repo.current()?;
    if current.head != after.head {
        return Err(ConcurvError::new(
            ErrorCode::StaleHead,
            "this workspace's head no longer matches the operation to undo; nothing was changed",
        ));
    }
    let description = format!("undo {} {target_id}: {}", target.kind, target.description);

    // Which files must the working copy hold afterwards, and what must it
    // hold now (to be sure no unsaved work is overwritten)?
    let files: Option<(SnapshotId, Option<SnapshotId>, bool)> = match target.kind.as_str() {
        // A save changed only the head: files stay as they are.
        "save" => None,
        // An update or branch switch moved files and head from `before.head`
        // to `after.head`.
        "update" | "merge" | "branch-switch" => {
            let b = before.head.ok_or_else(|| {
                refuse(&target.kind, "it was the first checkout of this workspace")
            })?;
            // The working copy must still be what the update wrote: the new
            // head, or the conflict tree of a pending merge.
            Some((b, target.checked_out, true))
        }
        // A restore changed only files: from the backup (or the clean head)
        // to the restored snapshot.
        _ => {
            let restored = target.checked_out.ok_or_else(|| {
                refuse(
                    "restore",
                    "it was recorded by an older build without its target",
                )
            })?;
            let back_to = target
                .backup
                .or(before.head)
                .ok_or_else(|| refuse("restore", "there is no earlier state to return to"))?;
            Some((back_to, Some(restored), false))
        }
    };

    match files {
        Some((back_to, expected, set_head)) => {
            let out = checkout(
                repo,
                &ws_lock,
                back_to,
                Checkout {
                    force: false,
                    set_head,
                    kind: "undo",
                    description,
                    expected,
                    undoes: Some(target_id),
                    // Restores the pending-merge state too (e.g. undoing a
                    // conflicted update clears it).
                    set_merge: Some(before.merge),
                    // Undoing a branch switch also restores the tracked branch.
                    set_branch: (target.kind == "branch-switch").then(|| before.branch.clone()),
                    transactional: target.kind == "branch-switch",
                },
            )?;
            Ok(UndoOutcome {
                undone: target_id,
                undone_kind: target.kind,
                undone_description: target.description,
                head: if set_head {
                    Some(back_to)
                } else {
                    current.head
                },
                files_restored_to: Some(back_to),
                written: out.written,
                deleted: out.deleted,
                operation: out.operation,
            })
        }
        None => {
            let lock = repo.lock()?;
            let (op_id, op, now) = repo.current()?;
            if now.head != after.head {
                return Err(ConcurvError::new(
                    ErrorCode::StaleHead,
                    "this workspace's head changed during the undo; nothing was changed",
                ));
            }
            let mut view = op.view.clone();
            if let Some(w) = view.workspaces.get_mut(&ws_id) {
                w.head = before.head;
                // Undoing the save that completed a merge brings the pending
                // merge back (its conflicts are already resolved on disk).
                w.merge = before.merge;
            }
            let new_op = Operation {
                parents: vec![op_id],
                timestamp_ms: repo.now_ms(),
                device_id: repo.device_id(),
                workspace_id: Some(ws_id),
                kind: "undo".into(),
                description,
                backup: None,
                undoes: Some(target_id),
                checked_out: None,
                view,
            };
            let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
            Ok(UndoOutcome {
                undone: target_id,
                undone_kind: target.kind,
                undone_description: target.description,
                head: before.head,
                files_restored_to: None,
                written: 0,
                deleted: 0,
                operation,
            })
        }
    }
}
