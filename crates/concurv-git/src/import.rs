//! `concurv git import`: read a Git branch through `git fast-export` into a
//! fresh concurv repository.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

use concurv_core::{BlobId, ConcurvError, ErrorCode, RepoPath, Result, SnapshotId};
use concurv_objects::{EntryKind, ObjectStore, Operation, Snapshot};
use concurv_workspace::worktree::{FileState, FlatTree, write_trees};
use concurv_workspace::{Repo, update};

use crate::git::{GitRepo, finish};
use crate::map::GitMap;
use crate::meta::{change_for_git_commit, concurv_author, parse_message};
use crate::quote::unquote;

/// Result of an import.
#[derive(Debug)]
pub struct ImportOutcome {
    /// Git branch imported.
    pub branch: String,
    /// Git commit at the branch head.
    pub git_tip: String,
    /// Snapshot now at `main` and checked out in this workspace.
    pub tip: SnapshotId,
    /// Commits imported.
    pub commits: usize,
    /// Files written into the working copy.
    pub written: usize,
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

/// Import `refs/heads/<branch>` of `git` into this (fresh) repository: all
/// commits become snapshots, `main` points at the branch head, and the
/// current workspace checks it out.
pub fn import(repo: &Repo, git: &GitRepo, branch: &str) -> Result<ImportOutcome> {
    let (_, op, ws) = repo.current()?;
    if op.view.branches.contains_key("main") || ws.head.is_some() {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            "git import needs a fresh repository: `main` and this workspace must be empty \
             (importing into existing history is not supported yet)",
        ));
    }
    let refname = format!("refs/heads/{branch}");
    let git_tip = git.resolve(&refname)?.ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::NotFound,
            format!("Git repository has no branch `{branch}`"),
        )
    })?;

    let writer = repo.store_writer()?;
    let store = writer.store();
    let mut child = git.spawn([
        "fast-export",
        "--full-tree",
        "--show-original-ids",
        "--reencode=no",
        "--signed-tags=strip",
        "--tag-of-filtered-object=drop",
        "--use-done-feature",
        &refname,
    ])?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| ConcurvError::internal("git stdout unavailable"))?;
    let parsed = parse_stream(store, BufReader::new(stdout));
    let finished = finish(child, "fast-export");
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
    let tip = *parsed
        .by_oid
        .get(&git_tip)
        .ok_or_else(|| proto("branch head missing from the export"))?;

    // Publish the imported head as `main` (CAS: must still be empty).
    {
        let lock = repo.lock()?;
        let (op_id, op, _) = repo.current()?;
        if op.view.branches.contains_key("main") {
            return Err(ConcurvError::new(
                ErrorCode::StaleHead,
                "`main` was published while importing; nothing was changed",
            ));
        }
        let mut view = op.view.clone();
        view.branches.insert("main".to_owned(), tip);
        let new_op = Operation {
            parents: vec![op_id],
            timestamp_ms: repo.now_ms(),
            device_id: repo.device_id(),
            workspace_id: Some(repo.workspace_id()),
            kind: "git-import".into(),
            description: format!(
                "import Git branch {branch} at {git_tip} ({} commits) from {}",
                parsed.commits,
                git.path().display()
            ),
            backup: None,
            undoes: None,
            checked_out: None,
            view,
        };
        repo.oplog().commit(&lock, &op_id, &new_op)?;
    }
    let mut map = GitMap::load(repo)?;
    for (oid, s) in &parsed.by_oid {
        map.insert(*s, oid.clone());
    }
    map.save(repo)?;
    drop(writer);
    let u = update(repo)?;
    Ok(ImportOutcome {
        branch: branch.to_owned(),
        git_tip,
        tip,
        commits: parsed.commits,
        written: u.written,
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

fn parse_stream<R: BufRead>(store: &ObjectStore, r: R) -> Result<Parsed> {
    let mut lines = Lines { r, pending: None };
    let mut marks: HashMap<u64, Mark> = HashMap::new();
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
