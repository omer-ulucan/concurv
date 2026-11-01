//! `concurv status` and `concurv diff`: read-only comparison of the working copy
//! with the workspace head.

use std::fs::File;
use std::io::Read;

use concurv_core::{ConcurvError, ConflictId, RepoPath, Result, SnapshotId};
use concurv_objects::EntryKind;

use crate::conflicts::{ConflictStatus, conflict_status};
use crate::repo::Repo;
use crate::restore::{RestoreMarker, read_marker};
use crate::worktree::{FileState, FlatTree, ScanMode, scan};

/// Kind of a working-copy change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Present in the working copy only.
    Added,
    /// Present only in the head.
    Deleted,
    /// Content changed (kind unchanged).
    Modified,
    /// Kind changed (file ↔ executable ↔ symlink), content may differ too.
    TypeChanged,
}

impl ChangeKind {
    /// Stable lowercase name used in JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
            Self::TypeChanged => "type_changed",
        }
    }
}

/// One changed path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// Path.
    pub path: RepoPath,
    /// What changed.
    pub kind: ChangeKind,
    /// Old state (head).
    pub old: Option<FileState>,
    /// New state (working copy).
    pub new: Option<FileState>,
}

/// Compare two flat maps; result is sorted by path.
pub fn diff_flat(old: &FlatTree, new: &FlatTree) -> Vec<Change> {
    let mut out = Vec::new();
    for (path, o) in old {
        match new.get(path) {
            None => out.push(Change {
                path: path.clone(),
                kind: ChangeKind::Deleted,
                old: Some(*o),
                new: None,
            }),
            Some(n) if n != o => out.push(Change {
                path: path.clone(),
                kind: if n.kind == o.kind {
                    ChangeKind::Modified
                } else {
                    ChangeKind::TypeChanged
                },
                old: Some(*o),
                new: Some(*n),
            }),
            Some(_) => {}
        }
    }
    for (path, n) in new {
        if !old.contains_key(path) {
            out.push(Change {
                path: path.clone(),
                kind: ChangeKind::Added,
                old: None,
                new: Some(*n),
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// A pending merge as seen by `status`.#[derive(Debug)]pub struct MergeStatus {    /// The conflict object.    pub id: ConflictId,    /// Branch head being merged.    pub theirs: SnapshotId,    /// State of every conflicted path.    pub conflicts: Vec<ConflictStatus>,}
/// A pending merge as seen by `status`.
#[derive(Debug)]
pub struct MergeStatus {
    /// The conflict object.
    pub id: ConflictId,
    /// Branch head being merged.
    pub theirs: SnapshotId,
    /// State of every conflicted path.
    pub conflicts: Vec<ConflictStatus>,
}

/// Result of `status`.
#[derive(Debug)]
pub struct StatusReport {
    /// Branch the workspace is on.
    pub branch: Option<String>,
    /// Workspace head snapshot.
    pub head: Option<SnapshotId>,
    /// Changes relative to the head.
    pub changes: Vec<Change>,
    /// Present if a restore was interrupted.
    pub interrupted_restore: Option<RestoreMarker>,
    /// Present while a merge with conflicts is pending (RFC 0004 §11).
    pub merge: Option<MergeStatus>,
}

/// Compute status without writing anything.
pub fn status(repo: &Repo) -> Result<StatusReport> {
    let (_, _, ws) = repo.current()?;
    let head_files = repo.head_files(ws.head.as_ref())?;
    let working = scan(repo.root(), ScanMode::HashOnly, &head_files)?;
    Ok(StatusReport {
        branch: ws.branch,
        head: ws.head,
        changes: diff_flat(&head_files, &working),
        interrupted_restore: read_marker(repo)?,
        merge: match ws.merge {
            Some(id) => {
                let conflict = repo.store().read_conflict(&id)?;
                let conflicts = conflict_status(repo, &conflict)?;
                Some(MergeStatus {
                    id,
                    theirs: conflict.theirs,
                    conflicts,
                })
            }
            None => None,
        },
    })
}

/// Largest file rendered as a text diff.
pub const MAX_TEXT_DIFF: u64 = 8 << 20;

/// Diff for one changed path.
#[derive(Debug)]
pub struct FileDiff {
    /// The change.
    pub change: Change,
    /// Unified diff text, or `None` if binary/too large/symlink.
    pub unified: Option<String>,
    /// Why no text diff was produced.
    pub note: Option<&'static str>,
}

fn as_text(bytes: Vec<u8>) -> Option<String> {
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Compute per-file diffs of the working copy against the head.
pub fn diff(repo: &Repo) -> Result<Vec<FileDiff>> {
    let st = status(repo)?;
    let mut out = Vec::new();
    for change in st.changes {
        let old = match &change.old {
            Some(o) if o.kind != EntryKind::Symlink => {
                let (_, len) = repo.store().verify(&o.blob.object_id())?;
                if len > MAX_TEXT_DIFF {
                    None
                } else {
                    Some(repo.store().read_blob(&o.blob)?)
                }
            }
            Some(_) => None,
            None => Some(Vec::new()),
        };
        let new = match &change.new {
            Some(n) if n.kind != EntryKind::Symlink => {
                let p = change.path.to_native(repo.root());
                let f = File::open(&p).map_err(|e| ConcurvError::io(e, "open", &p))?;
                let mut buf = Vec::new();
                f.take(MAX_TEXT_DIFF + 1)
                    .read_to_end(&mut buf)
                    .map_err(|e| ConcurvError::io(e, "read", &p))?;
                (buf.len() as u64 <= MAX_TEXT_DIFF).then_some(buf)
            }
            Some(_) => None,
            None => Some(Vec::new()),
        };
        let symlink = [change.old, change.new]
            .iter()
            .flatten()
            .any(|s| s.kind == EntryKind::Symlink);
        let (unified, note) = match (old, new) {
            _ if symlink => (None, Some("symlink target changed")),
            (Some(o), Some(n)) => match (as_text(o), as_text(n)) {
                (Some(o), Some(n)) => {
                    let path = change.path.to_string();
                    let text = similar::TextDiff::from_lines(&o, &n)
                        .unified_diff()
                        .context_radius(3)
                        .header(&format!("a/{path}"), &format!("b/{path}"))
                        .to_string();
                    (Some(text), None)
                }
                _ => (None, Some("binary files differ")),
            },
            _ => (None, Some("file too large for a text diff")),
        };
        out.push(FileDiff {
            change,
            unified,
            note,
        });
    }
    Ok(out)
}
