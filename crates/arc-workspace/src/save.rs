//! `arc save`: the journaled snapshot transaction (RFC 0002 §3).

use arc_core::{ArcError, ChangeId, OperationId, Result, SnapshotId};
use arc_objects::{Operation, Snapshot};
use arc_refs::RepoLock;

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

/// Store the working copy durably and return the flat map and snapshot.
/// Does not move any ref. Used by `save` and by restore's backup.
pub(crate) fn store_working_copy(
    repo: &Repo,
    _lock: &RepoLock,
    parent: Option<SnapshotId>,
    head_files: &FlatTree,
    message: &str,
) -> Result<(FlatTree, SnapshotId)> {
    let working = scan(repo.root(), ScanMode::Store(repo.store()), head_files)?;
    let tree = write_trees(repo.store(), &working)?;
    let snap = Snapshot {
        tree,
        parents: parent.into_iter().collect(),
        change_id: ChangeId::generate()?,
        timestamp_ms: repo.now_ms(),
        author: repo.author(),
        message: message.to_owned(),
    };
    let id = repo.store().write_snapshot(&snap)?;
    Ok((working, id))
}

/// Snapshot the working copy and advance the workspace head and its branch.
pub fn save(repo: &Repo, message: &str) -> Result<SaveOutcome> {
    if message.trim().is_empty() {
        return Err(ArcError::new(
            arc_core::ErrorCode::InvalidArgument,
            "a non-empty message is required (`arc save -m \"...\"`)",
        ));
    }
    let lock = repo.lock()?;
    let (op_id, op, ws) = repo.current()?;
    let head_files = repo.head_files(ws.head.as_ref())?;
    let (working, snap) = store_working_copy(repo, &lock, ws.head, &head_files, message)?;
    let changes = diff_flat(&head_files, &working);
    if changes.is_empty() && ws.head.is_some() {
        // The snapshot object is unreferenced and harmless; no operation recorded.
        return Ok(SaveOutcome {
            snapshot: None,
            operation: None,
            changes,
        });
    }
    let mut view = op.view.clone();
    if let Some(branch) = &ws.branch {
        view.branches.insert(branch.clone(), snap);
    }
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
    // A completed save records the full working copy, so any interrupted
    // restore is now superseded (its backup stays reachable in the op log).
    crate::restore::clear_marker(repo, &lock)?;
    Ok(SaveOutcome {
        snapshot: Some(snap),
        operation: Some(new_op_id),
        changes,
    })
}
