//! `concurv publish` and `concurv update` (RFC 0004 §6, §7).

use concurv_core::{
    AttestationId, ChangeId, ConcurvError, ConflictId, ErrorCode, OperationId, RepoPath, Result,
    SnapshotId,
};
use concurv_objects::{Conflict, ConflictEntry, EntryKind, ObjectStore, Operation, Snapshot};

use crate::conflicts::{Labels, build_worktree};
use crate::graph::{MergeBase, is_ancestor, merge_base};
use crate::merge::{MergeConflict, merge_partial_with};
use crate::repo::Repo;
use crate::restore::{Checkout, checkout};
use crate::textmerge::{MAX_MERGE_BYTES, merge3};
use crate::worktree::{FileState, FlatTree, write_trees};

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
    /// The attestation that authorized publishing to a protected branch
    /// (RFC 0005), if the branch is protected.
    pub attestation: Option<AttestationId>,
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
            attestation: None,
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
    // R4 (RFC 0005 §6): a protected branch needs a passing attestation for
    // exactly this candidate, under the policy of the branch's current head.
    let mut attestation = None;
    if let crate::policy::Protection::Required { policy_id, policy } =
        crate::policy::protection(repo.store(), &branch, from)?
    {
        let (found, reasons) =
            crate::check::find_authorizing(repo, head, &branch, policy_id, &policy)?;
        match found {
            Some(id) => attestation = Some(id),
            None => {
                let detail = if reasons.is_empty() {
                    "no attestation is recorded for it".to_owned()
                } else {
                    format!(
                        "no recorded attestation qualifies:\n  {}",
                        reasons.join("\n  ")
                    )
                };
                return Err(ConcurvError::new(
                    ErrorCode::PolicyFail,
                    format!(
                        "`{branch}` is protected by its policy ({}): publishing {head} needs a passing \
                         attestation, and {detail}. Run `concurv check` (or `concurv publish --check`); \
                         nothing was published",
                        &policy_id.to_hex()[..12]
                    ),
                ));
            }
        }
    }
    let mut view = op.view.clone();
    view.branches.insert(branch.clone(), head);
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "publish".into(),
        description: match attestation {
            Some(a) => format!("publish {head} to {branch} (attestation {a})"),
            None => format!("publish {head} to {branch}"),
        },
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
        attestation,
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
    /// Files changed on both sides whose edits were merged line by line
    /// (ADR 0006), sorted.
    pub content_merged: Vec<RepoPath>,
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
    integrate(repo, None, opts)
}

/// The snapshot a remote-tracking reference `remote/<remote>/<branch>`
/// names (RFC 0003 §5), if it exists. Local branch names can never start
/// with `remote/` (ADR 0007 §5), so this never shadows a local branch.
pub fn remote_tracking_head(view: &concurv_objects::View, name: &str) -> Option<SnapshotId> {
    let (remote, branch) = name.strip_prefix("remote/")?.split_once('/')?;
    view.remotes.get(remote)?.get(branch).map(|r| r.head)
}

/// `concurv merge <branch>` (ADR 0007 §2): integrate **another** branch into
/// this workspace, with exactly the rules of [`update_with`] (clean working
/// copy, exact merge base, line-level merge, conflicts refused or recorded).
/// The workspace keeps tracking its own branch, so only a following
/// `publish` (a compare-and-swap) can make the result a published head.
/// Merging the tracked branch itself is `update`, and is refused here.
/// `from` may also be a remote-tracking reference `remote/<remote>/<branch>`
/// (RFC 0003 §5 rule 3: integrating fetched work that diverged).
pub fn merge_branch(repo: &Repo, from: &str, opts: UpdateOptions) -> Result<UpdateOutcome> {
    integrate(repo, Some(from), opts)
}

fn integrate(repo: &Repo, from: Option<&str>, opts: UpdateOptions) -> Result<UpdateOutcome> {
    let ws_lock = repo.workspace_lock()?;
    let (_, op, ws) = repo.current()?;
    if ws.merge.is_some() {
        return Err(ConcurvError::new(
            ErrorCode::Conflict,
            "a merge is already in progress in this workspace: resolve the conflicts and \
             `concurv save`, or `concurv update --abort`",
        ));
    }
    let tracking = from.and_then(|b| remote_tracking_head(&op.view, b));
    let branch = match from {
        Some(b) if tracking.is_none() && !op.view.branches.contains_key(b) => {
            return Err(ConcurvError::new(
                ErrorCode::NotFound,
                format!(
                    "no branch named `{b}` (see `concurv branch list`; fetched branches are \
                     `remote/<remote>/<branch>`)"
                ),
            ));
        }
        Some(b) if ws.branch.as_deref() == Some(b) => {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!("this workspace already tracks `{b}`; use `concurv update` to bring it in"),
            ));
        }
        Some(b) => b.to_owned(),
        None => workspace_branch(ws.branch.clone())?,
    };
    let kind_name: &'static str = if from.is_some() { "merge" } else { "update" };
    let main = tracking.or_else(|| op.view.branches.get(&branch).copied());
    let up_to_date = |kind| UpdateOutcome {
        branch: branch.clone(),
        kind,
        head: ws.head,
        written: 0,
        deleted: 0,
        operation: None,
        conflict: None,
        content_merged: Vec::new(),
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
    let mut content_merged = Vec::new();
    let (target, kind, description) = if fast_forward {
        (
            m,
            UpdateKind::FastForward,
            format!("{kind_name}: fast-forward to {branch} {m}"),
        )
    } else {
        let w = ws
            .head
            .ok_or_else(|| ConcurvError::internal("diverged without a head"))?;
        let (base_id, base_files) = merge_base_files(repo, w, m, &branch)?;
        let ours = repo.snapshot_files(&w)?;
        let theirs = repo.snapshot_files(&m)?;
        let writer = repo.store_writer()?;
        let partial = merge_partial_with(&base_files, &ours, &theirs, &mut |path, b, o, t| {
            let merged = merge_contents(repo.store(), writer.store(), b, o, t)?;
            if merged.is_some() {
                content_merged.push(path.clone());
            }
            Ok(merged)
        })?;
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
                format!("{kind_name}: merge {branch} {m} into {w} as {snap}"),
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
                    "{kind_name}: merge {branch} {m} into {w} with {} conflict(s), pending as {cid}",
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
            kind: kind_name,
            description,
            expected: None,
            undoes: None,
            set_merge,
            set_branch: None,
            transactional: false,
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
        content_merged,
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

/// Line-level merge of one path changed on both sides (ADR 0006). Only
/// regular files (plain or executable) whose three versions are text are
/// merged; the executable bit merges like a file-level change. `None` keeps
/// the conflict. The merged blob is stored through `writer`.
fn merge_contents(
    store: &ObjectStore,
    writer: &ObjectStore,
    b: &FileState,
    o: &FileState,
    t: &FileState,
) -> Result<Option<FileState>> {
    let regular = |s: &FileState| matches!(s.kind, EntryKind::File | EntryKind::Executable);
    if !(regular(b) && regular(o) && regular(t)) {
        return Ok(None);
    }
    let kind = if o.kind == t.kind || t.kind == b.kind {
        o.kind
    } else if o.kind == b.kind {
        t.kind
    } else {
        return Ok(None);
    };
    let blob = if o.blob == t.blob || t.blob == b.blob {
        o.blob
    } else if o.blob == b.blob {
        t.blob
    } else {
        let mut texts = Vec::with_capacity(3);
        for s in [b, o, t] {
            let (_, len) = store.verify(&s.blob.object_id())?;
            if len > MAX_MERGE_BYTES as u64 {
                return Ok(None);
            }
            texts.push(store.read_blob(&s.blob)?);
        }
        match merge3(&texts[0], &texts[1], &texts[2]) {
            Some(bytes) => writer.write_blob_bytes(&bytes)?,
            None => return Ok(None),
        }
    };
    Ok(Some(FileState { kind, blob }))
}
