//! Tags (ADR 0008): shared, named references to snapshots that do not
//! move. Stored in the operation view (`View.tags`, repository format 4).

use concurv_core::{ConcurvError, ErrorCode, OperationId, Result, SnapshotId};
use concurv_objects::Operation;

use crate::{Repo, refname};

/// Repository format that may carry tags.
pub const TAGS_FORMAT: u32 = 4;

/// One tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagInfo {
    /// Tag name.
    pub name: String,
    /// Snapshot it names.
    pub target: SnapshotId,
}

/// Result of `tag create` or `tag delete`.
#[derive(Clone, Debug)]
pub struct TagOutcome {
    /// The tag.
    pub tag: TagInfo,
    /// Operation recorded.
    pub operation: OperationId,
    /// Whether the repository was upgraded to format 4 by this command.
    pub upgraded: bool,
}

fn rename_error(e: ConcurvError) -> ConcurvError {
    ConcurvError::new(e.code, e.message.replacen("branch name", "tag name", 1))
}

/// All rules for a new tag name, given the existing tags (ADR 0008 §3).
pub fn check_new_name<'a>(
    name: &str,
    existing: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    concurv_objects::encoding::validate_branch_name(name).map_err(rename_error)?;
    refname::portable(name).map_err(rename_error)?;
    if let Some(e) = refname::namespace_conflict(name, existing) {
        let msg = if e == name {
            format!("a tag named `{name}` already exists (tags do not move); nothing was changed")
        } else {
            format!(
                "tag name `{name}` collides with existing tag `{e}` (same name ignoring case, or \
                 one is a `/` prefix of the other, which Git cannot store side by side); nothing \
                 was changed"
            )
        };
        return Err(ConcurvError::new(ErrorCode::InvalidArgument, msg));
    }
    Ok(())
}

/// All tags, sorted by name.
pub fn list(repo: &Repo) -> Result<Vec<TagInfo>> {
    let (_, op, _) = repo.current()?;
    Ok(op
        .view
        .tags
        .iter()
        .map(|(name, target)| TagInfo {
            name: name.clone(),
            target: *target,
        })
        .collect())
}

/// Resolve `--at`: a branch name, then a tag name, then a snapshot id
/// prefix (ADR 0008 §2). A name that is a branch and a tag at different
/// snapshots is refused as ambiguous.
pub fn resolve(repo: &Repo, s: &str) -> Result<SnapshotId> {
    let (_, op, _) = repo.current()?;
    let branch = op.view.branches.get(s).copied();
    let tag = op.view.tags.get(s).copied();
    match (branch, tag) {
        (Some(b), Some(t)) if b != t => Err(ConcurvError::new(
            ErrorCode::AmbiguousId,
            format!(
                "`{s}` is both a branch (at {b}) and a tag (at {t}); pass the snapshot id instead"
            ),
        )),
        (Some(h), _) | (None, Some(h)) => Ok(h),
        (None, None) => repo.resolve_snapshot(s),
    }
}

/// Create tag `name` at `at` (default: this workspace's head, else the
/// head of its branch). Upgrades the repository to format 4 first.
pub fn create(repo: &Repo, name: &str, at: Option<SnapshotId>) -> Result<TagOutcome> {
    let (_, op, ws) = repo.current()?;
    check_new_name(name, op.view.tags.keys())?;
    let default_at = ws.head.or_else(|| {
        ws.branch
            .as_ref()
            .and_then(|b| op.view.branches.get(b).copied())
    });
    let target = at.or(default_at).ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::InvalidArgument,
            "nothing to tag: save a snapshot first, or pass --at",
        )
    })?;
    // A reference may only name a complete snapshot.
    repo.snapshot_files(&target)?;
    let lock = repo.lock()?;
    let upgraded = repo.upgrade_format(&lock, TAGS_FORMAT)?;
    let (op_id, op, _) = repo.current()?;
    // Re-check under the lock: decides concurrent creations.
    check_new_name(name, op.view.tags.keys())?;
    let mut view = op.view.clone();
    view.tags.insert(name.to_owned(), target);
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "tag-create".into(),
        description: format!("create tag {name} at {target}"),
        backup: None,
        undoes: None,
        checked_out: None,
        view,
    };
    let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
    Ok(TagOutcome {
        tag: TagInfo {
            name: name.to_owned(),
            target,
        },
        operation,
        upgraded,
    })
}

/// Delete tag `name`. The snapshot stays; the outcome names it.
pub fn delete(repo: &Repo, name: &str) -> Result<TagOutcome> {
    let lock = repo.lock()?;
    let (op_id, op, _) = repo.current()?;
    let Some(target) = op.view.tags.get(name).copied() else {
        return Err(ConcurvError::new(
            ErrorCode::NotFound,
            format!("no tag named `{name}` (see `concurv tag list`)"),
        ));
    };
    let mut view = op.view.clone();
    view.tags.remove(name);
    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "tag-delete".into(),
        description: format!("delete tag {name} (was at {target})"),
        backup: None,
        undoes: None,
        checked_out: None,
        view,
    };
    let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
    Ok(TagOutcome {
        tag: TagInfo {
            name: name.to_owned(),
            target,
        },
        operation,
        upgraded: false,
    })
}
