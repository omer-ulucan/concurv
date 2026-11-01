//! `concurv save`: snapshot the working copy and advance this workspace's
//! head (RFC 0002 §3, RFC 0004 §2, §5). Branches are not moved; see `publish`.

use concurv_core::{ChangeId, ConcurvError, ErrorCode, OperationId, Result, SnapshotId};
use concurv_objects::{ObjectStore, Operation, Snapshot};

use crate::repo::Repo;
use crate::status::{Change, diff_flat};
use crate::worktree::{FlatTree, ScanMode, scan, write_trees};

/// Result of a save.
#[derive(Debug)]
pub struct SaveOutcome {
    /// New snapshot, or `None` if the working copy equals the head.
    pub snapshot: Option<SnapshotId>,
    /// Operation recording the save.
    pub operation: Option<OperationId>,
    /// Changes relative to the previous head.
    pub changes: Vec<Change>,
}

/// Durably store the working copy and its snapshot through `store` (a
/// temp-session writer). Moves no pointer. Used by `save` and restore's backup.
/// The caller must hold the workspace lock.
pub(crate) fn store_working_copy(
    repo: &Repo,
    store: &ObjectStore,
    parent: Option<SnapshotId>,
    head_files: &FlatTree,
    message: &str,
) -> Result<(FlatTree, SnapshotId)> {
    let working = scan(repo.root(), ScanMode::Store(store), head_files)?;
    let tree = write_trees(store, &working)?;
    let snap = Snapshot {
        tree,
        parents: parent.into_iter().collect(),
        change_id: ChangeId::generate()?,
        timestamp_ms: repo.now_ms(),
        author: repo.author(),
        message: message.to_owned(),
    };
    let id = store.write_snapshot(&snap)?;
    Ok((working, id))
}

/// Snapshot the working copy and advance this workspace's head.
pub fn save(repo: &Repo, message: &str) -> Result<SaveOutcome> {
    if message.trim().is_empty() {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            "a non-empty message is required (`concurv save -m \"...\"`)",
        ));
    }
    let ws_lock = repo.workspace_lock()?;
    let (_, _, ws) = repo.current()?;
    let head_files = repo.head_files(ws.head.as_ref())?;

    // Lock-free phase: other workspaces may commit meanwhile.
    let writer = repo.store_writer()?;
    let (working, snap) = store_working_copy(repo, writer.store(), ws.head, &head_files, message)?;
    let changes = diff_flat(&head_files, &working);
    if changes.is_empty() && ws.head.is_some() {
        // The snapshot object is unreferenced and harmless; no operation recorded.
        return Ok(SaveOutcome {
            snapshot: None,
            operation: None,
            changes,
        });
    }

    // Commit phase: apply the change to the *current* view.
    let lock = repo.lock()?;
    let (op_id, op, current_ws) = repo.current()?;
    if current_ws.head != ws.head {
        return Err(ConcurvError::new(
            ErrorCode::StaleHead,
            "this workspace's head changed while saving; nothing was recorded, retry",
        ));
    }
    let mut view = op.view.clone();
    if let Some(w) = view.workspaces.get_mut(&repo.workspace_id()) {
        w.head = Some(snap);
    }
    let first_line = message.lines().next().unwrap_or("").trim();
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "save".into(),
        description: format!("save {snap}: {first_line}"),
        backup: None,
        view,
    };
    let new_op_id = repo.oplog().commit(&lock, &op_id, &new_op)?;
    drop(lock);
    // A completed save records the full working copy, so any interrupted
    // restore is now superseded (its backup stays reachable in the op log).
    crate::restore::clear_marker(repo, &ws_lock)?;
    Ok(SaveOutcome {
        snapshot: Some(snap),
        operation: Some(new_op_id),
        changes,
    })
}
