//! `concurv publish` and `concurv update` (RFC 0004 §6, §7).

use concurv_core::{
    ChangeId, ConcurvError, ConflictId, ErrorCode, OperationId, Result, SnapshotId,
};
use concurv_objects::{Conflict, ConflictEntry, Operation, Snapshot};

use crate::conflicts::{Labels, build_worktree};
use crate::graph::{MergeBase, is_ancestor, merge_base};
use crate::merge::{MergeConflict, merge_partial};
use crate::repo::Repo;
use crate::restore::{Checkout, checkout};
use crate::worktree::{FlatTree, write_trees};

/// Result of `publish`.
#[derive(Debug)]
pub struct PublishOutcome {
    /// Branch published to.
    pub branch: String,
    /// Branch head before (None if the branch did not exist yet).
    pub from: Option<SnapshotId>,
    /// Branch head after.
    pub to: SnapshotId,
    /// Operation recorded, or `None` if the branch was already at `to`.
    pub operation: Option<OperationId>,
}

fn workspace_branch(branch: Option<String>) -> Result<String> {
    branch.ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::InvalidArgument,
            "this workspace is not attached to a branch",
        )
    })
}

/// Fast-forward this workspace's branch to the workspace head with a CAS
/// under the repository lock. Never touches working files.
pub fn publish(repo: &Repo) -> Result<PublishOutcome> {
    let lock = repo.lock()?;
    let (op_id, op, ws) = repo.current()?;
    let branch = workspace_branch(ws.branch)?;
    let Some(head) = ws.head else {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            "nothing to publish: this workspace has no saved snapshot (`concurv save -m ...` first)",
        ));
    };
    let from = op.view.branches.get(&branch).copied();
    if from == Some(head) {
        return Ok(PublishOutcome {
            branch,
            from,
            to: head,
            operation: None,
        });
    }
    if let Some(m) = from
        && !is_ancestor(repo.store(), m, head)?
    {
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "`{branch}` is at {m}, which this workspace has not integrated; nothing was published. \
                 Run `concurv update`, check the result, then `concurv publish` again"
            ),
        ));
    }
    let mut view = op.view.clone();
    view.branches.insert(branch.clone(), head);
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "publish".into(),
        description: format!("publish {head} to {branch}"),
        backup: None,
        undoes: None,
        checked_out: None,
        view,
    };
    let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
    Ok(PublishOutcome {
        branch,
        from,
        to: head,
        operation: Some(operation),
    })
}

/// What `update` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateKind {
    /// The workspace already contains the branch head.
    UpToDate,
    /// The workspace head moved forward to the branch head.
    FastForward,
    /// A merge snapshot of the workspace head and branch head was created.
    Merged,
    /// The merge has conflicts; they were written into the working copy and
    /// recorded as a pending merge (`--allow-conflicts`, RFC 0004 §11).
    Conflicted,
}

impl UpdateKind {
    /// Stable lowercase name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::FastForward => "fast_forward",
            Self::Merged => "merged",
            Self::Conflicted => "conflicted",
        }
    }
}

/// Options for `update`.
#[derive(Clone, Copy, Debug, Default)]
pub struct UpdateOptions {
    /// Record conflicts as a pending merge instead of refusing.
    pub allow_conflicts: bool,
}

/// Result of `update`.
#[derive(Debug)]
pub struct UpdateOutcome {
    /// Branch integrated.
    pub branch: String,
    /// What happened.
    pub kind: UpdateKind,
    /// Workspace head after the update.
    pub head: Option<SnapshotId>,
    /// Files written in the working copy.
    pub written: usize,
    /// Files deleted from the working copy.
    pub deleted: usize,
    /// Operation recorded, if any.
    pub operation: Option<OperationId>,
    /// For [`UpdateKind::Conflicted`]: the pending merge and its conflicts.
    pub conflict: Option<(ConflictId, Vec<ConflictEntry>)>,
}

/// Error for a refused merge, listing every conflicting path.
fn conflict_error(branch: &str, conflicts: &[MergeConflict]) -> ConcurvError {
    let list: Vec<String> = conflicts
        .iter()
        .map(|c| format!("  {}: {}", c.path, c.kind))
        .collect();
    ConcurvError::new(
        ErrorCode::Conflict,
        format!(
            "cannot merge `{branch}` into this workspace: {} conflicting path(s); nothing was modified.\n{}\n\
             To resolve them: `concurv update --allow-conflicts` writes the conflicts into the working \
             copy for editing (see the `*.concurv-conflict` files), then `concurv save`",
            conflicts.len(),
            list.join("\n")
        ),
    )
}

/// Bring the branch head into this workspace (fast-forward or file-level
/// merge). Requires a clean working copy; never publishes. Conflicts are
/// refused; see [`update_with`].
pub fn update(repo: &Repo) -> Result<UpdateOutcome> {
    update_with(repo, UpdateOptions::default())
}

/// [`update`] with options.
pub fn update_with(repo: &Repo, opts: UpdateOptions) -> Result<UpdateOutcome> {
    let ws_lock = repo.workspace_lock()?;
    let (_, op, ws) = repo.current()?;
    if ws.merge.is_some() {
        return Err(ConcurvError::new(
            ErrorCode::Conflict,
            "a merge is already in progress in this workspace: resolve the conflicts and \
             `concurv save`, or `concurv update --abort`",
        ));
    }
    let branch = workspace_branch(ws.branch.clone())?;
    let main = op.view.branches.get(&branch).copied();
    let up_to_date = |kind| UpdateOutcome {
        branch: branch.clone(),
        kind,
        head: ws.head,
        written: 0,
        deleted: 0,
        operation: None,
        conflict: None,
    };
    let Some(m) = main else {
        return Ok(up_to_date(UpdateKind::UpToDate));
    };
    if let Some(w) = ws.head
        && is_ancestor(repo.store(), m, w)?
    {
        return Ok(up_to_date(UpdateKind::UpToDate));
    }

    let fast_forward = match ws.head {
        None => true,
        Some(w) => is_ancestor(repo.store(), w, m)?,
    };
    let mut set_head = true;
    let mut set_merge = None;
    let mut conflict = None;
    let (target, kind, description) = if fast_forward {
        (
            m,
            UpdateKind::FastForward,
            format!("update: fast-forward to {branch} {m}"),
        )
    } else {
        let w = ws
            .head
            .ok_or_else(|| ConcurvError::internal("diverged without a head"))?;
        let (base_id, base_files) = merge_base_files(repo, w, m, &branch)?;
        let ours = repo.snapshot_files(&w)?;
        let theirs = repo.snapshot_files(&m)?;
        let partial = merge_partial(&base_files, &ours, &theirs)?;
        let writer = repo.store_writer()?;
        if partial.conflicts.is_empty() {
            let tree = write_trees(writer.store(), &partial.merged)?;
            let snap = writer.store().write_snapshot(&Snapshot {
                tree,
                parents: vec![w, m],
                change_id: ChangeId::generate()?,
                timestamp_ms: repo.now_ms(),
                author: repo.author(),
                message: format!("merge {branch} into workspace {}", ws.name),
            })?;
            (
                snap,
                UpdateKind::Merged,
                format!("update: merge {branch} {m} into {w} as {snap}"),
            )
        } else if !opts.allow_conflicts {
            return Err(conflict_error(&branch, &partial.conflicts));
        } else {
            let labels = Labels {
                workspace: &ws.name,
                branch: &branch,
                ours: w,
                theirs: m,
                base: base_id,
            };
            let (files, entries) = build_worktree(
                writer.store(),
                &partial.merged,
                &base_files,
                &ours,
                &theirs,
                &partial.conflicts,
                &labels,
            )?;
            let tree = write_trees(writer.store(), &files)?;
            let worktree = writer.store().write_snapshot(&Snapshot {
                tree,
                parents: vec![w, m],
                change_id: ChangeId::generate()?,
                timestamp_ms: repo.now_ms(),
                author: repo.author(),
                message: format!(
                    "unresolved merge of {branch} into workspace {} (working copy for resolution)",
                    ws.name
                ),
            })?;
            let cid = writer.store().write_conflict(&Conflict {
                ours: w,
                theirs: m,
                base: base_id,
                worktree,
                entries: entries.clone(),
            })?;
            set_head = false;
            set_merge = Some(Some(cid));
            conflict = Some((cid, entries));
            (
                worktree,
                UpdateKind::Conflicted,
                format!(
                    "update: merge {branch} {m} into {w} with {} conflict(s), pending as {cid}",
                    partial.conflicts.len()
                ),
            )
        }
    };
    let out = checkout(
        repo,
        &ws_lock,
        target,
        Checkout {
            force: false,
            set_head,
            kind: "update",
            description,
            expected: None,
            undoes: None,
            set_merge,
        },
    )?;
    Ok(UpdateOutcome {
        branch,
        kind,
        head: if set_head { Some(target) } else { ws.head },
        written: out.written,
        deleted: out.deleted,
        operation: Some(out.operation),
        conflict,
    })
}

fn merge_base_files(
    repo: &Repo,
    w: SnapshotId,
    m: SnapshotId,
    branch: &str,
) -> Result<(Option<SnapshotId>, FlatTree)> {
    match merge_base(repo.store(), w, m)? {
        MergeBase::None => Ok((None, FlatTree::new())),
        MergeBase::One(b) => Ok((Some(b), repo.snapshot_files(&b)?)),
        MergeBase::Ambiguous(bases) => {
            let ids: Vec<String> = bases.iter().map(|b| b.to_hex()).collect();
            Err(ConcurvError::new(
                ErrorCode::Conflict,
                format!(
                    "cannot merge `{branch}`: the histories have {} equally good merge bases ({}), \
                     and choosing one could silently drop a change; nothing was modified",
                    ids.len(),
                    ids.join(", ")
                ),
            ))
        }
    }
}
