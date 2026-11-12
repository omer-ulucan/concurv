//! `concurv git import`: read a Git branch through `git fast-export` into a
//! concurv branch: into a fresh repository, or on top of history imported
//! or exported before (R6), using `.concurv/git-map` to recognise commits
//! concurv already has.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

use concurv_core::{BlobId, ConcurvError, ErrorCode, RepoPath, Result, SnapshotId};
use concurv_objects::{EntryKind, ObjectStore, Operation, Snapshot};
use concurv_workspace::branches::validate_new_branch_name;
use concurv_workspace::graph::ancestors;
use concurv_workspace::worktree::{FileState, FlatTree, write_trees};
use concurv_workspace::{Repo, tags, update};

use crate::git::{GitRepo, finish};
use crate::map::GitMap;
use crate::meta::{change_for_git_commit, concurv_author, parse_message};
use crate::quote::unquote;

/// What to import.
#[derive(Clone, Debug)]
pub struct ImportOptions {
    /// Git branch to read.
    pub branch: String,
    /// concurv branch to create or fast-forward (default `main`).
    pub into: String,
    /// Also import the Git tags that name commits of the imported history.
    pub tags: bool,
}

/// Result of an import.
#[derive(Debug)]
pub struct ImportOutcome {
    /// Git branch imported.
    pub branch: String,
    /// concurv branch created or fast-forwarded.
    pub into: String,
    /// Git commit at the branch head.
    pub git_tip: String,
    /// Snapshot of the Git branch head, now the head of `into`.
    pub tip: SnapshotId,
    /// Commits newly imported (commits concurv already had are not counted).
    pub commits: usize,
    /// Whether `into` was created or moved.
    pub published: bool,
    /// Whether this workspace (tracking `into`, clean, at its old head) was
    /// brought to the new head.
    pub checked_out: bool,
    /// Files written into the working copy.
    pub written: usize,
    /// Tags created: name and snapshot.
    pub tags_created: Vec<(String, SnapshotId)>,
    /// Git tags that concurv already had at the same snapshot.
    pub tags_unchanged: usize,
    /// Git tags not imported, with the reason.
    pub tags_skipped: Vec<(String, String)>,
    /// Annotated Git tags imported as lightweight tags (message and tagger
    /// dropped).
    pub annotated_dropped: usize,
}

/// Most problems listed before giving up.
const MAX_PROBLEMS: usize = 50;

struct Lines<R: BufRead> {
    r: R,
    pending: Option<Vec<u8>>,
}

fn proto(what: impl Into<String>) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::ExternalTool,
        format!("unexpected git fast-export output: {}", what.into()),
    )
}

impl<R: BufRead> Lines<R> {
    fn next(&mut self) -> Result<Option<Vec<u8>>> {
        if let Some(l) = self.pending.take() {
            return Ok(Some(l));
        }
        let mut buf = Vec::new();
        let n = self
            .r
            .read_until(b'\n', &mut buf)
            .map_err(|e| proto(e.to_string()))?;
        if n == 0 {
            return Ok(None);
        }
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        Ok(Some(buf))
    }
    fn push_back(&mut self, l: Vec<u8>) {
        self.pending = Some(l);
    }
    fn data_len(line: &[u8]) -> Result<u64> {
        let s = std::str::from_utf8(line)
            .ok()
            .and_then(|l| l.strip_prefix("data "))
            .ok_or_else(|| {
                proto(format!(
                    "expected `data`, got {:?}",
                    String::from_utf8_lossy(line)
                ))
            })?;
        s.parse().map_err(|_| proto("bad data length"))
    }
    fn read_data(&mut self, line: &[u8], max: u64) -> Result<Vec<u8>> {
        let n = Self::data_len(line)?;
        if n > max {
            return Err(proto("data block larger than allowed"));
        }
        let mut buf = vec![0u8; n as usize];
        self.r
            .read_exact(&mut buf)
            .map_err(|e| proto(e.to_string()))?;
        Ok(buf)
    }
}

fn mark_of(s: &str) -> Option<u64> {
    s.strip_prefix(':')?.parse().ok()
}

/// Parse `Name <email> <secs> <tz>`.
fn parse_ident(rest: &str) -> Option<(String, i64)> {
    // The timezone is not represented: concurv stores UTC milliseconds.
    let (who, _tz) = rest.rsplit_once(' ')?;
    let (who, secs) = who.rsplit_once(' ')?;
    let secs: i64 = secs.parse().ok()?;
    let lt = who.rfind(" <")?;
    let name = &who[..lt];
    let email = who[lt + 2..].strip_suffix('>')?;
    Some((concurv_author(name, email), secs.saturating_mul(1000)))
}

/// Import Git branch `branch` into concurv's `main` ([`import_with`]).
pub fn import(repo: &Repo, git: &GitRepo, branch: &str) -> Result<ImportOutcome> {
    import_with(
        repo,
        git,
        &ImportOptions {
            branch: branch.to_owned(),
            into: concurv_workspace::DEFAULT_BRANCH.to_owned(),
            tags: false,
        },
    )
}

/// Commits concurv already has (from `.concurv/git-map`) that exist in
/// `git`, as fast-export marks: they are not exported again, and commits
/// built on them refer to them by mark.
fn known_marks(repo: &Repo, git: &GitRepo, map: &GitMap) -> Result<Vec<(String, SnapshotId)>> {
    let store = repo.store();
    let candidates: Vec<(String, SnapshotId)> = map
        .iter()
        .filter(|(s, _)| store.read_snapshot(s).is_ok())
        .map(|(s, oid)| (oid.clone(), *s))
        .collect();
    let oids: Vec<String> = candidates.iter().map(|(o, _)| o.clone()).collect();
    let present = crate::export::present_commits(git, &oids)?;
    let mut seen = HashSet::new();
    Ok(candidates
        .into_iter()
        .filter(|(o, _)| present.contains(o) && seen.insert(o.clone()))
        .collect())
}

/// One Git tag: name, peeled commit, whether it is annotated.
fn git_tags(git: &GitRepo) -> Result<Vec<(String, String, bool)>> {
    let list = git.run_ok(
        [
            "for-each-ref",
            "--format=%(objecttype) %(refname)",
            "refs/tags",
        ],
        "list tags",
    )?;
    let refs: Vec<(String, bool)> = list
        .lines()
        .filter_map(|l| {
            let (kind, name) = l.split_once(' ')?;
            Some((name.to_owned(), kind == "tag"))
        })
        .collect();
    if refs.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = git.spawn(["cat-file", "--batch-check=%(objectname) %(objecttype)"])?;
    let wrote = child
        .stdin
        .take()
        .ok_or_else(|| ConcurvError::internal("git stdin unavailable"))
        .and_then(|mut stdin| {
            for (r, _) in &refs {
                writeln!(stdin, "{r}^{{commit}}")
                    .map_err(|e| ConcurvError::io(e, "write to git", git.path()))?;
            }
            Ok(())
        });
    let out = finish(child, "cat-file");
    wrote?;
    let out = String::from_utf8_lossy(&out?).into_owned();
    Ok(refs
        .into_iter()
        .zip(out.lines())
        .filter_map(|((r, annotated), line)| {
            let commit = line.strip_suffix(" commit")?;
            Some((
                r.strip_prefix("refs/tags/")?.to_owned(),
                commit.to_owned(),
                annotated,
            ))
        })
        .collect())
}

/// Import `refs/heads/<opts.branch>` of `git` into concurv branch
/// `opts.into` (ADR 0002, R6): commits concurv already has (recorded in
/// `.concurv/git-map` by earlier imports and exports) are reused, new ones
/// become snapshots. `into` is created, or moved forward only if the Git
/// branch contains its head; otherwise the import is refused
/// (`CONCURV_DIVERGED`): import into a new branch and `concurv merge` it.
/// This workspace is brought to the new head only if it tracks `into`, was
/// at its old head and has no unsaved changes; no other file changes.
/// With `opts.tags`, Git tags naming commits of the imported history become
/// concurv tags in the same operation (ADR 0008 §6). All or nothing.
pub fn import_with(repo: &Repo, git: &GitRepo, opts: &ImportOptions) -> Result<ImportOutcome> {
    let (branch, into) = (opts.branch.as_str(), opts.into.as_str());
    let (_, op, ws) = repo.current()?;
    let into_before = op.view.branches.get(into).copied();
    if into_before.is_none() {
        validate_new_branch_name(into, op.view.branches.keys())?;
    }
    let refname = format!("refs/heads/{branch}");
    let git_tip = git.resolve(&refname)?.ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::NotFound,
            format!("Git repository has no branch `{branch}`"),
        )
    })?;

    let mut map = GitMap::load(repo)?;
    let known = known_marks(repo, git, &map)?;
    let writer = repo.store_writer()?;
    let store = writer.store();
    let marks_file = store.tmp_dir().join("git-import-marks");
    let mut marks_text = String::new();
    let mut seeded = HashMap::new();
    for (i, (oid, snap)) in known.iter().enumerate() {
        let mark = i as u64 + 1;
        marks_text.push_str(&format!(":{mark} {oid}\n"));
        seeded.insert(mark, *snap);
    }
    std::fs::write(&marks_file, &marks_text)
        .map_err(|e| ConcurvError::io(e, "write fast-export marks", &marks_file))?;
    let mut child = git.spawn([
        "fast-export".to_owned(),
        "--full-tree".to_owned(),
        "--show-original-ids".to_owned(),
        "--reencode=no".to_owned(),
        "--signed-tags=strip".to_owned(),
        "--tag-of-filtered-object=drop".to_owned(),
        "--use-done-feature".to_owned(),
        format!("--import-marks={}", marks_file.display()),
        refname.clone(),
    ])?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ConcurvError::internal("git stdout unavailable"))?;
    let parsed = parse_stream(store, BufReader::new(stdout), seeded);
    let finished = finish(child, "fast-export");
    let _ = std::fs::remove_file(&marks_file);
    let parsed = parsed?;
    finished?;
    if !parsed.problems.is_empty() {
        let n = parsed.problems.len();
        return Err(ConcurvError::new(
            ErrorCode::UnsupportedFileType,
            format!(
                "cannot import `{branch}` without losing information; nothing was changed. \
                 {n}{} problem(s):\n  {}",
                if n >= MAX_PROBLEMS { "+" } else { "" },
                parsed.problems.join("\n  ")
            ),
        ));
    }
    let by_oid: HashMap<&str, SnapshotId> = known
        .iter()
        .map(|(o, s)| (o.as_str(), *s))
        .chain(parsed.by_oid.iter().map(|(o, s)| (o.as_str(), *s)))
        .collect();
    let tip = *by_oid
        .get(git_tip.as_str())
        .ok_or_else(|| proto("branch head missing from the export"))?;
    let history = ancestors(store, tip)?;

    // `into` may only move forward.
    if let Some(h) = into_before
        && h != tip
        && !history.contains_key(&h)
    {
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "concurv branch `{into}` has snapshots that Git branch `{branch}` does not contain; \
                 nothing was changed. Import into a new branch (`--into <name>`), then \
                 `concurv merge <name>` and publish"
            ),
        ));
    }

    // Tags of the imported history.
    let mut tags_created: Vec<(String, SnapshotId)> = Vec::new();
    let mut tags_unchanged = 0;
    let mut tags_skipped: Vec<(String, String)> = Vec::new();
    let mut annotated_dropped = 0;
    if opts.tags {
        let mut conflicts = Vec::new();
        let mut names: Vec<String> = op.view.tags.keys().cloned().collect();
        for (name, commit, annotated) in git_tags(git)? {
            let Some(target) = by_oid
                .get(commit.as_str())
                .copied()
                .filter(|s| history.contains_key(s))
            else {
                tags_skipped.push((name, "not in the imported history".into()));
                continue;
            };
            match op.view.tags.get(&name) {
                Some(t) if *t == target => tags_unchanged += 1,
                Some(t) => conflicts.push(format!("{name} (concurv: {t}, Git: {commit})")),
                None => match tags::check_new_name(&name, names.iter()) {
                    Ok(()) => {
                        names.push(name.clone());
                        annotated_dropped += usize::from(annotated);
                        tags_created.push((name, target));
                    }
                    Err(e) => tags_skipped.push((name, e.message)),
                },
            }
        }
        if !conflicts.is_empty() {
            return Err(ConcurvError::new(
                ErrorCode::Diverged,
                format!(
                    "concurv already has tag(s) with these names at other snapshots: {}; tags never \
                     move, so nothing was changed. Delete or rename the tag on one side",
                    conflicts.join(", ")
                ),
            ));
        }
    }

    // Record the import (CAS against the state read at the start).
    let published = into_before != Some(tip);
    if published || !tags_created.is_empty() {
        let lock = repo.lock()?;
        if !tags_created.is_empty() {
            repo.upgrade_format(&lock, tags::TAGS_FORMAT)?;
        }
        let (op_id, now, _) = repo.current()?;
        if now.view.branches.get(into).copied() != into_before
            || tags_created
                .iter()
                .any(|(n, _)| now.view.tags.contains_key(n))
        {
            return Err(ConcurvError::new(
                ErrorCode::StaleHead,
                format!("`{into}` or its tags changed while importing; nothing was changed, retry"),
            ));
        }
        let mut view = now.view.clone();
        view.branches.insert(into.to_owned(), tip);
        for (n, t) in &tags_created {
            view.tags.insert(n.clone(), *t);
        }
        let new_op = Operation {
            parents: vec![op_id],
            timestamp_ms: repo.now_ms(),
            device_id: repo.device_id(),
            workspace_id: Some(repo.workspace_id()),
            kind: "git-import".into(),
            description: format!(
                "import Git branch {branch} at {git_tip} into {into} ({} new commits{}) from {}",
                parsed.commits,
                if tags_created.is_empty() {
                    String::new()
                } else {
                    format!(", {} tags", tags_created.len())
                },
                git.path().display()
            ),
            backup: None,
            undoes: None,
            checked_out: None,
            view,
        };
        repo.oplog().commit(&lock, &op_id, &new_op)?;
    }
    for (oid, s) in &parsed.by_oid {
        map.insert(*s, oid.clone());
    }
    map.save(repo)?;
    drop(writer);

    // Bring this workspace along only if that cannot lose anything.
    let mut checked_out = false;
    let mut written = 0;
    if published && ws.branch.as_deref() == Some(into) && ws.head == into_before {
        match update(repo) {
            Ok(u) => {
                checked_out = true;
                written = u.written;
            }
            Err(e) if e.code == ErrorCode::DirtyWorkspace => {}
            Err(e) => return Err(e),
        }
    }
    tags_created.sort();
    Ok(ImportOutcome {
        branch: branch.to_owned(),
        into: into.to_owned(),
        git_tip,
        tip,
        commits: parsed.commits,
        published,
        checked_out,
        written,
        tags_created,
        tags_unchanged,
        tags_skipped,
        annotated_dropped,
    })
}

struct Parsed {
    by_oid: HashMap<String, SnapshotId>,
    commits: usize,
    problems: Vec<String>,
}

enum Mark {
    Blob(BlobId),
    Commit(SnapshotId),
}

/// Parse a fast-export stream; `seeded` marks name commits concurv already
/// has (passed to fast-export with `--import-marks`).
fn parse_stream<R: BufRead>(
    store: &ObjectStore,
    r: R,
    seeded: HashMap<u64, SnapshotId>,
) -> Result<Parsed> {
    let mut lines = Lines { r, pending: None };
    let mut marks: HashMap<u64, Mark> = seeded
        .into_iter()
        .map(|(m, s)| (m, Mark::Commit(s)))
        .collect();
    let mut out = Parsed {
        by_oid: HashMap::new(),
        commits: 0,
        problems: Vec::new(),
    };
    let problem = |out: &mut Parsed, p: String| {
        if out.problems.len() < MAX_PROBLEMS {
            out.problems.push(p);
        }
    };
    while let Some(line) = lines.next()? {
        let text = String::from_utf8_lossy(&line).into_owned();
        if text.is_empty() || text.starts_with("feature ") || text.starts_with("progress ") {
            continue;
        }
        if text == "done" {
            break;
        }
        if text == "blob" {
            let mark = lines
                .next()?
                .and_then(|l| mark_of(std::str::from_utf8(&l).ok()?.strip_prefix("mark ")?));
            let mark = mark.ok_or_else(|| proto("blob without mark"))?;
            let mut data = lines.next()?.ok_or_else(|| proto("truncated blob"))?;
            if data.starts_with(b"original-oid ") {
                data = lines.next()?.ok_or_else(|| proto("truncated blob"))?;
            }
            let n = Lines::<R>::data_len(&data)?;
            let blob =
                store.write_blob_stream((&mut lines.r).take(n), n, Path::new("<git blob>"))?;
            marks.insert(mark, Mark::Blob(blob));
            continue;
        }
        if text.starts_with("reset ") {
            // A reset may carry a `from`; branch tips come from original ids.
            if let Some(next) = lines.next()?
                && !next.starts_with(b"from ")
            {
                lines.push_back(next);
            }
            continue;
        }
        if text.starts_with("tag ") {
            // Tags of the branch are not imported; skip the block.
            while let Some(l) = lines.next()? {
                if l.starts_with(b"data ") {
                    lines.read_data(&l, 16 << 20)?;
                    break;
                }
            }
            continue;
        }
        if !text.starts_with("commit ") {
            return Err(proto(format!("unsupported command {text:?}")));
        }

        // ---- commit
        let mut oid = String::new();
        let mut author: Option<(String, i64)> = None;
        let mut mark = None;
        let message;
        loop {
            let l = lines.next()?.ok_or_else(|| proto("truncated commit"))?;
            let t = String::from_utf8_lossy(&l).into_owned();
            if let Some(m) = t.strip_prefix("mark ") {
                mark = mark_of(m);
            } else if let Some(o) = t.strip_prefix("original-oid ") {
                oid = o.to_owned();
            } else if let Some(a) = t.strip_prefix("author ") {
                author = parse_ident(a);
                if author.is_none() {
                    problem(&mut out, format!("commit {oid}: unparseable author {a:?}"));
                }
            } else if t.starts_with("committer ") {
                // The committer is not represented in concurv (ADR 0002).
            } else if let Some(enc) = t.strip_prefix("encoding ") {
                if !enc.eq_ignore_ascii_case("utf-8") && !enc.eq_ignore_ascii_case("utf8") {
                    problem(
                        &mut out,
                        format!("commit {oid}: message encoding {enc} is not UTF-8"),
                    );
                }
            } else if t.starts_with("data ") {
                message = lines.read_data(&l, 16 << 20)?;
                break;
            } else {
                return Err(proto(format!("unexpected commit header {t:?}")));
            }
        }
        let mark = mark.ok_or_else(|| proto("commit without mark"))?;
        let mut parents = Vec::new();
        let mut files = FlatTree::new();
        let mut ok = true;
        while let Some(l) = lines.next()? {
            if l.is_empty() {
                break;
            }
            let t = String::from_utf8_lossy(&l).into_owned();
            if let Some(p) = t.strip_prefix("from ").or_else(|| t.strip_prefix("merge ")) {
                match mark_of(p).and_then(|m| marks.get(&m)) {
                    Some(Mark::Commit(s)) => parents.push(*s),
                    _ => {
                        problem(
                            &mut out,
                            format!("commit {oid}: parent {p} is not part of the branch export"),
                        );
                        ok = false;
                    }
                }
            } else if t == "deleteall" {
                files.clear();
            } else if l.starts_with(b"M ") {
                // M <mode> <dataref> <path>
                let mut parts = l[2..].splitn(3, |b| *b == b' ');
                let mode = parts.next().unwrap_or_default().to_vec();
                let dataref =
                    String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
                let raw_path = parts.next().unwrap_or_default();
                let path_bytes = unquote(raw_path)?;
                let shown = String::from_utf8_lossy(&path_bytes).into_owned();
                let kind = match mode.as_slice() {
                    b"100644" | b"100664" | b"644" => Some(EntryKind::File),
                    b"100755" | b"755" => Some(EntryKind::Executable),
                    b"120000" => Some(EntryKind::Symlink),
                    b"160000" => {
                        problem(
                            &mut out,
                            format!("commit {oid}: `{shown}` is a submodule (not supported)"),
                        );
                        None
                    }
                    other => {
                        problem(
                            &mut out,
                            format!(
                                "commit {oid}: `{shown}` has unsupported mode {}",
                                String::from_utf8_lossy(other)
                            ),
                        );
                        None
                    }
                };
                let path = match String::from_utf8(path_bytes)
                    .ok()
                    .and_then(|s| RepoPath::parse(&s).ok().filter(|p| !p.is_root()))
                {
                    Some(p) => Some(p),
                    None => {
                        problem(
                            &mut out,
                            format!(
                                "commit {oid}: path `{shown}` is not allowed in concurv (invalid name, `.concurv`, or not UTF-8)"
                            ),
                        );
                        None
                    }
                };
                let blob = match mark_of(&dataref).and_then(|m| marks.get(&m)) {
                    Some(Mark::Blob(b)) => Some(*b),
                    _ => {
                        if kind.is_some() {
                            problem(
                                &mut out,
                                format!(
                                    "commit {oid}: content of `{shown}` missing from the export"
                                ),
                            );
                        }
                        None
                    }
                };
                match (kind, path, blob) {
                    (Some(kind), Some(path), Some(blob)) => {
                        files.insert(path, FileState { kind, blob });
                    }
                    _ => ok = false,
                }
            } else if let Some(p) = t.strip_prefix("D ") {
                if let Some(path) = String::from_utf8(unquote(p.as_bytes())?)
                    .ok()
                    .and_then(|s| RepoPath::parse(&s).ok())
                {
                    files.remove(&path);
                }
            } else if t.starts_with("N ") {
                // Notes are not imported.
            } else {
                // A command without the blank-line terminator.
                lines.push_back(l);
                break;
            }
        }
        out.commits += 1;
        let message = match String::from_utf8(message) {
            Ok(m) => m,
            Err(_) => {
                problem(
                    &mut out,
                    format!("commit {oid}: message is not valid UTF-8"),
                );
                ok = false;
                String::new()
            }
        };
        if !ok || !out.problems.is_empty() {
            continue;
        }
        let Some((author, timestamp_ms)) = author.clone() else {
            continue;
        };
        let (message, change) = parse_message(&message);
        let change_id = match change.or_else(|| change_for_git_commit(&oid)) {
            Some(c) => c,
            None => concurv_core::ChangeId::generate()?,
        };
        let tree = write_trees(store, &files)?;
        let snap = store.write_snapshot(&Snapshot {
            tree,
            parents,
            change_id,
            timestamp_ms,
            author,
            message,
        })?;
        marks.insert(mark, Mark::Commit(snap));
        if !oid.is_empty() {
            out.by_oid.insert(oid, snap);
        }
    }
    Ok(out)
}
