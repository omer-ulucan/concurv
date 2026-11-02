//! Pending merges with recorded conflicts (RFC 0004 §11).
//!
//! `update --allow-conflicts` writes every conflict into the working copy in a
//! form a human or agent can resolve, and records an immutable [`Conflict`]
//! object in the workspace's view entry. The merge completes when no side
//! file or marker remains and the user runs `save`.

use std::fs::File;
use std::io::Read;

use concurv_core::{ConcurvError, ConflictId, ErrorCode, RepoPath, Result, SnapshotId};
use concurv_objects::{Conflict, ConflictEntry, ConflictKind, EntryKind, ObjectStore};

use crate::merge::MergeConflict;
use crate::repo::Repo;
use crate::restore::{Checkout, RestoreOutcome, checkout};
use crate::worktree::{FileState, FlatTree, Present, stat_path};

/// First line of the "ours" section of a text conflict.
pub const MARKER_OURS: &str = "<<<<<<< ours";
/// First line of the "base" section.
pub const MARKER_BASE: &str = "||||||| base";
/// Separator before the "theirs" section.
pub const MARKER_SEP: &str = "=======";
/// Last line of a text conflict.
pub const MARKER_THEIRS: &str = ">>>>>>> theirs";
/// Suffix of the explanation side file written for every conflicted path.
pub const SIDE_CONFLICT: &str = ".concurv-conflict";
/// Suffix of the side file holding the other side's version.
pub const SIDE_THEIRS: &str = ".concurv-theirs";

/// Largest file for which text markers are written.
const MAX_MARKER_FILE: u64 = 8 << 20;

/// Labels used in markers and explanations.
pub(crate) struct Labels<'a> {
    pub workspace: &'a str,
    pub branch: &'a str,
    pub ours: SnapshotId,
    pub theirs: SnapshotId,
    pub base: Option<SnapshotId>,
}

fn short(s: &SnapshotId) -> String {
    s.to_hex()[..12].to_owned()
}

/// Content of a text file version, if it is text suitable for markers.
fn text_of(store: &ObjectStore, st: Option<&FileState>) -> Result<Option<String>> {
    let Some(st) = st else {
        return Ok(Some(String::new()));
    };
    if st.kind == EntryKind::Symlink {
        return Ok(None);
    }
    let (_, len) = store.verify(&st.blob.object_id())?;
    if len > MAX_MARKER_FILE {
        return Ok(None);
    }
    let bytes = store.read_blob(&st.blob)?;
    if bytes.contains(&0) {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok())
}

fn section(out: &mut String, header: &str, body: &str) {
    out.push_str(header);
    out.push('\n');
    out.push_str(body);
    if !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
}

fn side_path(path: &RepoPath, suffix: &str) -> Result<RepoPath> {
    let comps = path.components();
    let (last, parent) = comps
        .split_last()
        .ok_or_else(|| ConcurvError::internal("conflict on the root path"))?;
    let parent = RepoPath::parse(&parent.join("/"))?;
    parent.join(&format!("{last}{suffix}")).map_err(|e| {
        ConcurvError::new(
            ErrorCode::Conflict,
            format!(
                "cannot write a conflict side file for `{path}`: {}",
                e.message
            ),
        )
    })
}

fn describe(st: Option<&FileState>) -> &'static str {
    match st {
        None => "absent (deleted or never added)",
        Some(s) if s.kind == EntryKind::Symlink => "a symlink",
        Some(_) => "present",
    }
}

/// Build the working tree for a conflicted merge: the clean part of the
/// merge plus, for every conflict, markers or the kept "ours" version and
/// side files. Blobs are written through `store`. Returns the tree's files
/// and the conflict entries.
pub(crate) fn build_worktree(
    store: &ObjectStore,
    clean: &FlatTree,
    base: &FlatTree,
    ours: &FlatTree,
    theirs: &FlatTree,
    conflicts: &[MergeConflict],
    labels: &Labels<'_>,
) -> Result<(FlatTree, Vec<ConflictEntry>)> {
    if let Some(c) = conflicts
        .iter()
        .find(|c| c.kind == ConflictKind::FileDirectory)
    {
        return Err(ConcurvError::new(
            ErrorCode::Conflict,
            format!(
                "`{}` is a file on one side and a directory on the other; concurv cannot \
                 represent that conflict in the working copy yet. Nothing was modified",
                c.path
            ),
        ));
    }
    let mut files = clean.clone();
    let mut entries = Vec::new();
    let occupied = |p: &RepoPath, files: &FlatTree| {
        files.contains_key(p)
            || base.contains_key(p)
            || ours.contains_key(p)
            || theirs.contains_key(p)
    };
    for c in conflicts {
        let (b, o, t) = (base.get(&c.path), ours.get(&c.path), theirs.get(&c.path));
        let conflict_file = side_path(&c.path, SIDE_CONFLICT)?;
        let theirs_file = side_path(&c.path, SIDE_THEIRS)?;
        for p in [&conflict_file, &theirs_file] {
            if occupied(p, &files) {
                return Err(ConcurvError::new(
                    ErrorCode::Conflict,
                    format!(
                        "cannot write conflict side file `{p}`: the path is already in use. Nothing was modified"
                    ),
                ));
            }
        }
        let texts = match (o, t) {
            (Some(_), Some(_)) => (text_of(store, b)?, text_of(store, o)?, text_of(store, t)?),
            _ => (None, None, None),
        };
        let mut side_files = vec![conflict_file.clone()];
        let markers = if let (Some(bt), Some(ot), Some(tt)) = texts {
            let mut s = String::new();
            section(
                &mut s,
                &format!(
                    "{MARKER_OURS} (workspace {}, snapshot {})",
                    labels.workspace,
                    short(&labels.ours)
                ),
                &ot,
            );
            let base_label = labels
                .base
                .map_or("none".to_owned(), |b| format!("snapshot {}", short(&b)));
            section(&mut s, &format!("{MARKER_BASE} ({base_label})"), &bt);
            section(&mut s, MARKER_SEP, &tt);
            s.push_str(&format!(
                "{MARKER_THEIRS} ({}, snapshot {})\n",
                labels.branch,
                short(&labels.theirs)
            ));
            let kind = o.map_or(EntryKind::File, |x| x.kind);
            files.insert(
                c.path.clone(),
                FileState {
                    kind,
                    blob: store.write_blob_bytes(s.as_bytes())?,
                },
            );
            true
        } else {
            if let Some(o) = o {
                files.insert(c.path.clone(), *o);
            }
            if let Some(t) = t {
                let content = store.read_blob(&t.blob)?;
                files.insert(
                    theirs_file.clone(),
                    FileState {
                        kind: EntryKind::File,
                        blob: store.write_blob_bytes(&content)?,
                    },
                );
                side_files.push(theirs_file.clone());
            }
            false
        };
        let how = if markers {
            format!(
                "  Edit `{p}`: keep what is right and remove the lines starting with\n  `{MARKER_OURS}`, `{MARKER_BASE}`, `{MARKER_SEP}` and `{MARKER_THEIRS}`.\n",
                p = c.path
            )
        } else {
            format!(
                "  `{p}` holds ours ({}); theirs ({}) is {}.\n  Keep, replace or delete `{p}` as needed.\n",
                describe(o),
                describe(t),
                if t.is_some() {
                    format!("in `{theirs_file}`")
                } else {
                    "absent".to_owned()
                },
                p = c.path
            )
        };
        let note = format!(
            "concurv: merge conflict in `{p}` ({kind}).\n\n\
             ours   = workspace `{ws}`, snapshot {ours}: {od}\n\
             theirs = `{br}`, snapshot {theirs}: {td}\n\n\
             How to resolve:\n{how}\
             Then delete this file{extra}, and run `concurv save -m \"...\"`.\n\
             To give up the whole merge instead: `concurv update --abort`.\n",
            p = c.path,
            kind = c.kind,
            ws = labels.workspace,
            ours = labels.ours,
            od = describe(o),
            br = labels.branch,
            theirs = labels.theirs,
            td = describe(t),
            extra = if side_files.len() > 1 {
                format!(" and `{theirs_file}`")
            } else {
                String::new()
            },
        );
        files.insert(
            conflict_file,
            FileState {
                kind: EntryKind::File,
                blob: store.write_blob_bytes(note.as_bytes())?,
            },
        );
        entries.push(ConflictEntry {
            path: c.path.clone(),
            kind: c.kind,
            markers,
            side_files,
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((files, entries))
}

/// State of one conflicted path in the working copy.
#[derive(Clone, Debug)]
pub struct ConflictStatus {
    /// Conflicted path.
    pub path: RepoPath,
    /// Why it conflicted.
    pub kind: ConflictKind,
    /// Side files that still exist.
    pub remaining_side_files: Vec<RepoPath>,
    /// True if the path still contains conflict marker lines.
    pub markers_remaining: bool,
}

impl ConflictStatus {
    /// True once nothing of the conflict remains in the working copy.
    pub fn resolved(&self) -> bool {
        self.remaining_side_files.is_empty() && !self.markers_remaining
    }
}

fn has_markers(native: &std::path::Path) -> Result<bool> {
    let mut buf = Vec::new();
    match stat_path(native)? {
        Present::File { .. } => {
            File::open(native)
                .and_then(|f| f.take(MAX_MARKER_FILE * 3 + 4096).read_to_end(&mut buf))
                .map_err(|e| ConcurvError::io(e, "read", native))?;
        }
        _ => return Ok(false),
    }
    Ok(String::from_utf8_lossy(&buf).lines().any(|l| {
        l.starts_with(MARKER_OURS) || l.starts_with(MARKER_BASE) || l.starts_with(MARKER_THEIRS)
    }))
}

/// Check every conflicted path of `conflict` in the working copy.
pub fn conflict_status(repo: &Repo, conflict: &Conflict) -> Result<Vec<ConflictStatus>> {
    let mut out = Vec::new();
    for e in &conflict.entries {
        let mut remaining = Vec::new();
        for s in &e.side_files {
            if stat_path(&s.to_native(repo.root()))? != Present::Missing {
                remaining.push(s.clone());
            }
        }
        let markers_remaining = e.markers && has_markers(&e.path.to_native(repo.root()))?;
        out.push(ConflictStatus {
            path: e.path.clone(),
            kind: e.kind,
            remaining_side_files: remaining,
            markers_remaining,
        });
    }
    Ok(out)
}

/// Error listing unresolved conflicts.
pub(crate) fn unresolved_error(unresolved: &[&ConflictStatus]) -> ConcurvError {
    let list: Vec<String> = unresolved
        .iter()
        .map(|c| {
            let mut why = Vec::new();
            if c.markers_remaining {
                why.push("conflict markers remain".to_owned());
            }
            for s in &c.remaining_side_files {
                why.push(format!("`{s}` still exists"));
            }
            format!("  {}: {}", c.path, why.join(", "))
        })
        .collect();
    ConcurvError::new(
        ErrorCode::Conflict,
        format!(
            "a merge is in progress with {} unresolved conflict(s); nothing was saved.\n{}\n\
             Resolve them (see each `*{SIDE_CONFLICT}` file), then save again; or `concurv update --abort`",
            unresolved.len(),
            list.join("\n")
        ),
    )
}

/// Give up a pending merge: the working copy returns to the workspace head
/// (unsaved edits made since the conflicted update are kept in a backup
/// snapshot first) and the pending merge is cleared.
pub fn abort_merge(repo: &Repo) -> Result<RestoreOutcome> {
    let ws_lock = repo.workspace_lock()?;
    let (_, _, ws) = repo.current()?;
    let Some(cid) = ws.merge else {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            "no merge is in progress in this workspace",
        ));
    };
    let conflict = repo.store().read_conflict(&cid)?;
    checkout(
        repo,
        &ws_lock,
        conflict.ours,
        Checkout {
            force: true,
            set_head: false,
            kind: "merge-abort",
            description: format!("abort merge of {} (conflict {cid})", conflict.theirs),
            expected: Some(conflict.worktree),
            set_merge: Some(None),
            undoes: None,
            set_branch: None,
        },
    )
}

/// Load this workspace's pending merge, if any.
pub fn pending_merge(repo: &Repo) -> Result<Option<(ConflictId, Conflict)>> {
    let (_, _, ws) = repo.current()?;
    match ws.merge {
        Some(id) => Ok(Some((id, repo.store().read_conflict(&id)?))),
        None => Ok(None),
    }
}
