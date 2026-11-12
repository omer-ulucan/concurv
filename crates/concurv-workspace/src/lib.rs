//! The concurv local engine: repository lifecycle, private workspaces,
//! working-copy scanning, `save`, `status`/`diff`, safe `restore`,
//! `publish`/`update` with file-level merge, history and `verify`.

pub mod branches;
pub mod check;
pub mod conflicts;
pub mod coord;
pub mod graph;
pub mod ignore;
pub mod integrate;
pub mod merge;
pub mod policy;
pub mod reconcile;
pub mod refname;
pub mod repo;
pub mod restore;
pub mod save;
pub mod statcache;
pub mod status;
pub mod textmerge;
pub mod undo;
pub mod verify;
pub mod workspaces;
pub mod worktree;

pub use conflicts::{ConflictStatus, abort_merge, conflict_status, pending_merge};
pub use integrate::{
    PublishOutcome, UpdateKind, UpdateOptions, UpdateOutcome, merge_branch, publish, update,
    update_with,
};
pub use merge::{ConflictKind, MergeConflict, PartialMerge};
pub use repo::{DEFAULT_BRANCH, DEFAULT_WORKSPACE, Repo, RepoOptions};
pub use restore::{RestoreMarker, RestoreOptions, RestoreOutcome, restore};
pub use save::{SaveOutcome, save};
pub use status::{
    Change, ChangeKind, FileDiff, MergeStatus, StatusOptions, StatusReport, diff, diff_with,
    status, status_with,
};
pub use undo::{UndoOutcome, undo};
pub use verify::{Problem, VerifyReport, verify};
pub use workspaces::{AddOutcome, WorkspaceInfo};
pub use worktree::{FileState, FlatTree};

use concurv_core::{Result, SnapshotId};
use concurv_objects::Snapshot;

/// First-parent history of the workspace head, newest first.
pub fn log(repo: &Repo, limit: usize) -> Result<Vec<(SnapshotId, Snapshot)>> {
    let mut out = Vec::new();
    let mut cur = repo.head_snapshot()?;
    while let Some(id) = cur {
        if out.len() >= limit {
            break;
        }
        let s = repo.store().read_snapshot(&id)?;
        cur = s.parents.first().copied();
        out.push((id, s));
    }
    Ok(out)
}

/// First-parent operation history from the op head, newest first. Includes
/// `init`, `save` and `restore` operations and the backup snapshots recorded
/// by `restore --force`. Every operation is hash-verified as it is read.
pub fn op_log(
    repo: &Repo,
    limit: usize,
) -> Result<Vec<(concurv_core::OperationId, concurv_objects::Operation)>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    repo.oplog().walk(limit)
}
