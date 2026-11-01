//! The concurv R1 local engine: repository lifecycle, working-copy scanning,
//! `save`, `status`/`diff`, safe `restore`, history and `verify`.

pub mod repo;
pub mod restore;
pub mod save;
pub mod status;
pub mod verify;
pub mod worktree;

pub use repo::{DEFAULT_BRANCH, Repo, RepoOptions};
pub use restore::{RestoreMarker, RestoreOptions, RestoreOutcome, restore};
pub use save::{SaveOutcome, save};
pub use status::{Change, ChangeKind, FileDiff, StatusReport, diff, status};
pub use verify::{Problem, VerifyReport, verify};
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
