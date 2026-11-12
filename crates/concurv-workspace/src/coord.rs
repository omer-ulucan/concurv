//! Agent coordination (RFC 0006, R5): advisory intents with a TTL, overlap
//! warnings, and a versioned event stream. Everything here is advisory:
//! nothing refuses work because of an intent or a warning.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use concurv_core::{ConcurvError, ErrorCode, RandomId, RepoPath, Result, SnapshotId};
use concurv_objects::fs::{create_dir_durable, write_file_atomic};
use concurv_objects::{EntryKind, ObjectStore};
use serde::{Deserialize, Serialize};

use crate::graph::{MergeBase, is_ancestor, merge_base};
use crate::repo::Repo;
use crate::worktree::FlatTree;

/// Shortest and longest intent lifetime.
pub const MIN_TTL_MS: i64 = 60_000;
/// Longest intent lifetime (7 days).
pub const MAX_TTL_MS: i64 = 7 * 24 * 3_600_000;
/// Files larger than this are not read for symbol and line analysis.
const MAX_TEXT: u64 = 1 << 20;

/// An advisory declaration: who is about to change which paths or
/// symbols, until when. Never a lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    /// Schema version (1).
    pub concurv_intent: u32,
    /// Random id (32 hex digits).
    pub id: String,
    /// Device that declared it (hex).
    pub device: String,
    /// Workspace name.
    pub workspace: String,
    /// Author or agent name.
    pub who: String,
    /// Path patterns (ignore-file syntax).
    pub paths: Vec<String>,
    /// Symbol names.
    pub symbols: Vec<String>,
    /// Free text.
    pub note: String,
    /// Creation time (ms since the epoch).
    pub created_ms: i64,
    /// Expiry time.
    pub expires_ms: i64,
}

impl Intent {
    /// Still active at `now`.
    pub fn active(&self, now: i64) -> bool {
        self.expires_ms > now
    }

    /// Validate an intent received from elsewhere (a relay or a file).
    pub fn validate(&self) -> Result<()> {
        let bad = |why: &str| {
            ConcurvError::new(ErrorCode::InvalidArgument, format!("invalid intent: {why}"))
        };
        if self.concurv_intent != 1 {
            return Err(bad("unknown version"));
        }
        if RandomId::from_hex(&self.id).is_err() || RandomId::from_hex(&self.device).is_err() {
            return Err(bad("malformed id"));
        }
        if self.paths.is_empty() && self.symbols.is_empty() {
            return Err(bad("no paths and no symbols"));
        }
        if self.paths.len() > 64 || self.symbols.len() > 64 || self.note.len() > 1024 {
            return Err(bad("too large"));
        }
        if self.paths.iter().any(|p| p.is_empty() || p.len() > 512)
            || self.symbols.iter().any(|s| !is_identifier(s))
        {
            return Err(bad("invalid path pattern or symbol"));
        }
        if self.workspace.len() > 64 || self.who.len() > 256 {
            return Err(bad("name too long"));
        }
        if self.expires_ms <= self.created_ms || self.expires_ms - self.created_ms > MAX_TTL_MS {
            return Err(bad("invalid lifetime"));
        }
        Ok(())
    }
}

fn is_identifier(s: &str) -> bool {
    let mut b = s.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && s.len() <= 128
}

fn intents_dir(repo: &Repo) -> PathBuf {
    repo.store_dir().join("intents")
}

fn remote_intents_path(repo: &Repo, remote: &str) -> PathBuf {
    repo.store_dir()
        .join("remote-intents")
        .join(format!("{remote}.json"))
}

fn read_json_dir(dir: &Path) -> Vec<Intent> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter_map(|e| fs::read(e.path()).ok())
        .filter_map(|b| serde_json::from_slice::<Intent>(&b).ok())
        .filter(|i| i.validate().is_ok())
        .collect()
}

/// Declare an intent for this workspace. `ttl_ms` is clamped to 1 minute –
/// 7 days. Expired intents of the repository are removed on the way.
pub fn declare(
    repo: &Repo,
    paths: &[String],
    symbols: &[String],
    ttl_ms: i64,
    note: &str,
) -> Result<Intent> {
    let (_, _, ws) = repo.current()?;
    let now = repo.now_ms();
    let intent = Intent {
        concurv_intent: 1,
        id: RandomId::generate()?.to_hex(),
        device: repo.device_id().to_hex(),
        workspace: ws.name,
        who: repo.author(),
        paths: paths.to_vec(),
        symbols: symbols.to_vec(),
        note: note.to_owned(),
        created_ms: now,
        expires_ms: now + ttl_ms.clamp(MIN_TTL_MS, MAX_TTL_MS),
    };
    intent.validate()?;
    let _lock = repo.lock()?;
    let dir = intents_dir(repo);
    create_dir_durable(&dir, &repo.options().faults)?;
    prune(&dir, now);
    let body =
        serde_json::to_vec_pretty(&intent).map_err(|e| ConcurvError::internal(e.to_string()))?;
    write_file_atomic(
        repo.store().tmp_dir(),
        &dir.join(format!("{}.json", intent.id)),
        &body,
        &repo.options().faults,
    )?;
    Ok(intent)
}

fn prune(dir: &Path, now: i64) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let stale = fs::read(e.path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Intent>(&b).ok())
            .is_none_or(|i| !i.active(now));
        if stale {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Release this workspace's intents (all, or the one with `id`). Returns
/// how many were removed.
pub fn release(repo: &Repo, id: Option<&str>) -> Result<usize> {
    let (_, _, ws) = repo.current()?;
    let _lock = repo.lock()?;
    let dir = intents_dir(repo);
    let mut n = 0;
    for i in read_json_dir(&dir) {
        let mine = i.workspace == ws.name && i.device == repo.device_id().to_hex();
        if mine && id.is_none_or(|x| i.id.starts_with(x)) {
            let p = dir.join(format!("{}.json", i.id));
            fs::remove_file(&p).map_err(|e| ConcurvError::io(e, "remove", &p))?;
            n += 1;
        }
    }
    Ok(n)
}

/// Active intents of this repository's workspaces.
pub fn local_intents(repo: &Repo) -> Vec<Intent> {
    let now = repo.now_ms();
    read_json_dir(&intents_dir(repo))
        .into_iter()
        .filter(|i| i.active(now))
        .collect()
}

/// Active intents of other devices, as last fetched from `remote`.
pub fn remote_intents(repo: &Repo, remote: &str) -> Vec<Intent> {
    let now = repo.now_ms();
    fs::read(remote_intents_path(repo, remote))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<Intent>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.validate().is_ok() && i.active(now))
        .collect()
}

/// Every active intent known here: local ones and those fetched from
/// remotes (other devices only).
pub fn all_intents(repo: &Repo) -> Vec<Intent> {
    let mut v = local_intents(repo);
    let me = repo.device_id().to_hex();
    if let Ok(rd) = fs::read_dir(repo.store_dir().join("remote-intents")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(remote) = name.strip_suffix(".json") {
                v.extend(
                    remote_intents(repo, remote)
                        .into_iter()
                        .filter(|i| i.device != me),
                );
            }
        }
    }
    v.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.id.cmp(&b.id)));
    v.dedup_by(|a, b| a.id == b.id);
    v
}

/// Replace the cached intents of `remote` (called by `fetch`).
pub fn store_remote_intents(repo: &Repo, remote: &str, intents: &[Intent]) -> Result<()> {
    let p = remote_intents_path(repo, remote);
    if let Some(parent) = p.parent() {
        create_dir_durable(parent, &repo.options().faults)?;
    }
    let valid: Vec<&Intent> = intents.iter().filter(|i| i.validate().is_ok()).collect();
    let body = serde_json::to_vec(&valid).map_err(|e| ConcurvError::internal(e.to_string()))?;
    write_file_atomic(repo.store().tmp_dir(), &p, &body, &repo.options().faults)
}

// ------------------------------------------------------------------ events

/// The operation log as versioned events (RFC 0006 §1), oldest first; at
/// most the `limit` most recent operations.
pub fn events(repo: &Repo, limit: usize) -> Result<Vec<serde_json::Value>> {
    let ops = repo.oplog().walk(limit)?;
    let mut out = Vec::new();
    for (id, op) in ops.into_iter().rev() {
        let parent = match op.parents.first() {
            Some(p) => Some(repo.store().read_operation(p)?),
            None => None,
        };
        let mut moves = serde_json::Map::new();
        let before = parent.as_ref().map(|p| &p.view.branches);
        let names: BTreeSet<&String> = op
            .view
            .branches
            .keys()
            .chain(before.into_iter().flat_map(|b| b.keys()))
            .collect();
        for n in names {
            let from = before.and_then(|b| b.get(n));
            let to = op.view.branches.get(n);
            if from != to {
                moves.insert(
                    n.clone(),
                    serde_json::json!({
                        "from": from.map(|s| s.to_hex()),
                        "to": to.map(|s| s.to_hex()),
                    }),
                );
            }
        }
        let ws = op.workspace_id.and_then(|w| op.view.workspaces.get(&w));
        out.push(serde_json::json!({
            "concurv_event": 1,
            "type": "operation",
            "operation": id.to_hex(),
            "kind": op.kind,
            "time_ms": op.timestamp_ms,
            "device": op.device_id.to_hex(),
            "workspace": ws.map(|w| w.name.clone()),
            "branch_moves": moves,
            "head": ws.and_then(|w| w.head).map(|h| h.to_hex()),
            "description": op.description,
        }));
    }
    Ok(out)
}

// ---------------------------------------------------------------- overlaps

/// Kind of overlap warning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Both sides change the same file.
    Path,
    /// Their line edits overlap or touch: a merge would conflict.
    Conflict,
    /// A changed path or mentioned symbol matches someone's intent.
    Intent,
    /// Upstream work changed the definition of a symbol this workspace uses.
    Dependency,
}

impl Level {
    /// Stable lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Conflict => "conflict",
            Self::Intent => "intent",
            Self::Dependency => "dependency",
        }
    }
}

/// One overlap warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlap {
    /// Level.
    pub level: Level,
    /// Where the other work is: `workspace <name>`, `branch <b>`,
    /// `remote/<r>/<b>`, or `intent <id> (<who>)`.
    pub source: String,
    /// Path in this workspace's change set.
    pub path: String,
    /// The symbol involved (`intent` and `dependency`).
    pub symbol: Option<String>,
}

/// Base and new text of one changed file; `None` = absent or unreadable as
/// text (binary, symlink or too large).
pub type TextChange = (Option<Vec<u8>>, Option<Vec<u8>>);

/// A set of changed files with their before (base) and after contents.
#[derive(Default)]
pub struct ChangeSet {
    /// Path → (base content, new content).
    pub files: BTreeMap<RepoPath, TextChange>,
}

fn blob_text(store: &ObjectStore, files: &FlatTree, p: &RepoPath) -> Option<Vec<u8>> {
    let st = files.get(p)?;
    if st.kind == EntryKind::Symlink {
        return None;
    }
    store.read_blob(&st.blob).ok()
}

fn files_of(repo: &Repo, s: Option<SnapshotId>) -> Result<FlatTree> {
    match s {
        Some(s) => repo.snapshot_files(&s),
        None => Ok(FlatTree::new()),
    }
}

/// Changes from `base` to `head` (snapshots).
pub fn snapshot_changes(
    repo: &Repo,
    base: Option<SnapshotId>,
    head: SnapshotId,
) -> Result<ChangeSet> {
    let b = files_of(repo, base)?;
    let h = files_of(repo, Some(head))?;
    let mut cs = ChangeSet::default();
    let paths: BTreeSet<&RepoPath> = b.keys().chain(h.keys()).collect();
    for p in paths {
        if b.get(p) != h.get(p) {
            cs.files.insert(
                p.clone(),
                (
                    blob_text(repo.store(), &b, p),
                    blob_text(repo.store(), &h, p),
                ),
            );
        }
    }
    Ok(cs)
}

fn single_base(repo: &Repo, a: SnapshotId, b: SnapshotId) -> Result<Option<SnapshotId>> {
    Ok(match merge_base(repo.store(), a, b)? {
        MergeBase::One(x) => Some(x),
        MergeBase::None | MergeBase::Ambiguous(_) => None,
    })
}

/// This workspace's change set: from the merge base with its tracked
/// branch (or the empty tree) to the working copy, saved and unsaved.
pub fn my_changes(repo: &Repo) -> Result<(ChangeSet, Option<SnapshotId>)> {
    let (_, op, ws) = repo.current()?;
    let branch_head = ws
        .branch
        .as_ref()
        .and_then(|b| op.view.branches.get(b))
        .copied();
    let base = match (ws.head, branch_head) {
        (Some(h), Some(b)) => single_base(repo, h, b)?,
        _ => None,
    };
    let mut cs = match ws.head {
        Some(h) => snapshot_changes(repo, base, h)?,
        None => ChangeSet::default(),
    };
    let base_files = files_of(repo, base)?;
    let status = crate::status::status(repo)?;
    for c in status.changes {
        let disk = c.path.to_native(repo.root());
        let new = match fs::symlink_metadata(&disk) {
            Ok(m) if m.is_file() && m.len() <= MAX_TEXT => fs::read(&disk).ok(),
            _ => None,
        };
        let old = blob_text(repo.store(), &base_files, &c.path);
        if new.as_deref() == old.as_deref() && new.is_some() {
            cs.files.remove(&c.path);
        } else {
            cs.files.insert(c.path, (old, new));
        }
    }
    Ok((cs, base))
}

/// Line ranges `[start, end)` of `base` that `new` changes (an insertion
/// is an empty range at its position).
pub fn changed_ranges(base: &[u8], new: &[u8]) -> Option<Vec<(usize, usize)>> {
    let (b, n) = (
        std::str::from_utf8(base).ok()?,
        std::str::from_utf8(new).ok()?,
    );
    let diff = similar::TextDiff::from_lines(b, n);
    Some(
        diff.ops()
            .iter()
            .filter(|o| o.tag() != similar::DiffTag::Equal)
            .map(|o| (o.old_range().start, o.old_range().end))
            .collect(),
    )
}

/// Would edits `a` and `b` of the same base conflict? Line ranges that
/// overlap or touch (no unchanged line between them) conflict, as in the
/// merge rules of ADR 0006; anything that is not text conflicts.
pub fn predict_conflict(base: Option<&[u8]>, a: Option<&[u8]>, b: Option<&[u8]>) -> bool {
    match (base, a, b) {
        (Some(base), Some(a), Some(b)) => {
            let (Some(ra), Some(rb)) = (changed_ranges(base, a), changed_ranges(base, b)) else {
                return true;
            };
            ra.iter()
                .any(|x| rb.iter().any(|y| x.0.max(y.0) <= x.1.min(y.1)))
        }
        _ => a != b,
    }
}

const DEF_KEYWORDS: [&str; 10] = [
    "fn",
    "def",
    "function",
    "class",
    "struct",
    "enum",
    "trait",
    "interface",
    "type",
    "func",
];

/// Names defined on `line` (a definition keyword followed by an identifier).
fn defined_names(line: &str) -> Vec<String> {
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    let mut out = Vec::new();
    for w in words.windows(2) {
        if DEF_KEYWORDS.contains(&w[0]) && is_identifier(w[1]) && !DEF_KEYWORDS.contains(&w[1]) {
            out.push(w[1].to_owned());
        }
    }
    out
}

/// Symbols whose definition line changed between `base` and `new`.
pub fn changed_definitions(base: Option<&[u8]>, new: Option<&[u8]>) -> BTreeSet<String> {
    let text = |b: Option<&[u8]>| {
        b.and_then(|x| std::str::from_utf8(x).ok())
            .unwrap_or("")
            .to_owned()
    };
    let (b, n) = (text(base), text(new));
    let diff = similar::TextDiff::from_lines(&b, &n);
    let mut out = BTreeSet::new();
    for change in diff.iter_all_changes() {
        if change.tag() != similar::ChangeTag::Equal {
            out.extend(defined_names(change.value()));
        }
    }
    out
}

/// True if `text` mentions `symbol` as a whole word.
pub fn mentions(text: &[u8], symbol: &str) -> bool {
    let Ok(t) = std::str::from_utf8(text) else {
        return false;
    };
    t.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .any(|w| w == symbol)
}

/// Like [`mentions`], but ignoring line comments (`//`, `#` followed by a
/// space) and the contents of double-quoted string literals: a use in
/// code, not in prose.
pub fn mentions_in_code(text: &[u8], symbol: &str) -> bool {
    let Ok(t) = std::str::from_utf8(text) else {
        return false;
    };
    t.lines().any(|line| {
        let mut code = String::with_capacity(line.len());
        let mut in_str = false;
        let mut prev = '\0';
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if in_str {
                if c == '"' && prev != '\\' {
                    in_str = false;
                    code.push(' ');
                }
            } else if c == '"' {
                in_str = true;
            } else if (c == '/' && chars.peek() == Some(&'/'))
                || (c == '#' && matches!(chars.peek(), Some(' ') | None))
            {
                // A line comment (`//`, or `# ` as in shells and Python).
                break;
            } else {
                code.push(c);
            }
            prev = c;
        }
        mentions(code.as_bytes(), symbol)
    })
}

fn compare(mine: &ChangeSet, theirs: &ChangeSet, source: &str, out: &mut Vec<Overlap>) {
    for (p, (base, new)) in &mine.files {
        if let Some((tbase, tnew)) = theirs.files.get(p) {
            let base = base.as_deref().or(tbase.as_deref());
            let level = if predict_conflict(base, new.as_deref(), tnew.as_deref()) {
                Level::Conflict
            } else {
                Level::Path
            };
            out.push(Overlap {
                level,
                source: source.to_owned(),
                path: p.to_string(),
                symbol: None,
            });
        }
    }
    let defs: BTreeSet<String> = theirs
        .files
        .values()
        .flat_map(|(b, n)| changed_definitions(b.as_deref(), n.as_deref()))
        .collect();
    for (p, (_, new)) in &mine.files {
        let Some(new) = new else { continue };
        for d in &defs {
            if mentions_in_code(new, d) && !theirs.files.contains_key(p) {
                out.push(Overlap {
                    level: Level::Dependency,
                    source: source.to_owned(),
                    path: p.to_string(),
                    symbol: Some(d.clone()),
                });
            }
        }
    }
}

fn intent_matches(intent: &Intent, path: &str) -> bool {
    let mut b = ignore::gitignore::GitignoreBuilder::new("");
    for p in &intent.paths {
        if b.add_line(None, p).is_err() {
            return false;
        }
    }
    match b.build() {
        Ok(g) => g.matched_path_or_any_parents(path, false).is_ignore(),
        Err(_) => false,
    }
}

/// Overlap warnings for this workspace (RFC 0006 §3), sorted.
pub fn overlaps(repo: &Repo) -> Result<Vec<Overlap>> {
    let (_, op, ws) = repo.current()?;
    let (mine, _) = my_changes(repo)?;
    let mut out = Vec::new();
    if mine.files.is_empty() {
        return Ok(out);
    }
    // Other workspaces' unpublished saves.
    for (id, w) in &op.view.workspaces {
        if *id == repo.workspace_id() {
            continue;
        }
        let Some(h) = w.head else { continue };
        let their_branch = w
            .branch
            .as_ref()
            .and_then(|b| op.view.branches.get(b))
            .copied();
        if their_branch.is_some_and(|b| is_ancestor(repo.store(), h, b).unwrap_or(false)) {
            continue; // nothing unpublished
        }
        let base = match their_branch {
            Some(b) => single_base(repo, h, b)?,
            None => None,
        };
        let theirs = snapshot_changes(repo, base, h)?;
        compare(&mine, &theirs, &format!("workspace {}", w.name), &mut out);
    }
    // Upstream work this workspace has not integrated: its branch, and the
    // remote-tracking references of that branch.
    if let Some(b) = &ws.branch {
        let mut upstream: Vec<(String, SnapshotId)> = Vec::new();
        if let Some(h) = op.view.branches.get(b) {
            upstream.push((format!("branch {b}"), *h));
        }
        for (r, m) in &op.view.remotes {
            if let Some(t) = m.get(b)
                && Some(t.head) != op.view.branches.get(b).copied()
            {
                upstream.push((format!("remote/{r}/{b}"), t.head));
            }
        }
        for (label, up) in upstream {
            let ahead = match ws.head {
                Some(h) => !is_ancestor(repo.store(), up, h)?,
                None => true,
            };
            if !ahead {
                continue;
            }
            let base = match ws.head {
                Some(h) => single_base(repo, h, up)?,
                None => None,
            };
            let theirs = snapshot_changes(repo, base, up)?;
            compare(&mine, &theirs, &label, &mut out);
        }
    }
    // Intents of other workspaces and devices.
    let me = repo.device_id().to_hex();
    for i in all_intents(repo) {
        if i.device == me && i.workspace == ws.name {
            continue;
        }
        let source = format!("intent {} ({})", &i.id[..12], i.who);
        for (p, (_, new)) in &mine.files {
            let ps = p.to_string();
            if intent_matches(&i, &ps) {
                out.push(Overlap {
                    level: Level::Intent,
                    source: source.clone(),
                    path: ps.clone(),
                    symbol: None,
                });
            }
            for s in &i.symbols {
                if new.as_deref().is_some_and(|t| mentions(t, s)) {
                    out.push(Overlap {
                        level: Level::Intent,
                        source: source.clone(),
                        path: ps.clone(),
                        symbol: Some(s.clone()),
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| {
        (a.level, &a.source, &a.path, &a.symbol).cmp(&(b.level, &b.source, &b.path, &b.symbol))
    });
    out.dedup();
    Ok(out)
}
