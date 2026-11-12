//! `concurv git export`: write `main`'s history into a Git repository
//! through `git fast-import`, then move a Git branch with compare-and-swap.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufWriter, Read, Write};

use concurv_core::{BlobId, ConcurvError, ErrorCode, Result, SnapshotId};
use concurv_objects::EntryKind;
use concurv_workspace::Repo;
use concurv_workspace::graph::ancestors;

use crate::git::{GitRepo, finish};
use crate::map::GitMap;
use crate::meta::{git_ident, git_message};
use crate::quote::quote;

/// Private ref `fast-import` writes to; deleted after every export.
const EXPORT_REF: &str = "refs/concurv/export";

/// What to export.
#[derive(Clone, Debug)]
pub struct ExportOptions {
    /// concurv branch whose history is exported.
    pub from: String,
    /// Git branch to move.
    pub branch: String,
    /// Also export the tags that name snapshots of that history (ADR 0008).
    pub tags: bool,
}

/// A tag written to (or already in) Git.
#[derive(Clone, Debug)]
pub struct TagExport {
    /// Tag name (`refs/tags/<name>`).
    pub name: String,
    /// Git commit it names.
    pub commit: String,
    /// False if Git already had this tag at this commit.
    pub created: bool,
}

/// Result of an export.
#[derive(Debug)]
pub struct ExportOutcome {
    /// Git branch updated.
    pub branch: String,
    /// Git commit of `main`'s head.
    pub tip: String,
    /// Previous value of the Git branch.
    pub previous: Option<String>,
    /// Snapshots written as new Git commits.
    pub exported: usize,
    /// Snapshots already present in the Git repository.
    pub reused: usize,
    /// Whether the Git branch moved.
    pub updated: bool,
    /// Tags exported (with `tags`), sorted by name.
    pub tags: Vec<TagExport>,
    /// Tags not exported because their snapshot is not in the exported
    /// history.
    pub tags_skipped: Vec<String>,
}

/// Parents before children, deterministic.
fn topo_order(graph: &HashMap<SnapshotId, Vec<SnapshotId>>, tip: SnapshotId) -> Vec<SnapshotId> {
    let mut out = Vec::with_capacity(graph.len());
    let mut done: HashSet<SnapshotId> = HashSet::new();
    let mut stack: Vec<(SnapshotId, bool)> = vec![(tip, false)];
    while let Some((id, expanded)) = stack.pop() {
        if done.contains(&id) {
            continue;
        }
        if expanded {
            done.insert(id);
            out.push(id);
            continue;
        }
        stack.push((id, true));
        for p in graph.get(&id).into_iter().flatten().rev() {
            if !done.contains(p) {
                stack.push((*p, false));
            }
        }
    }
    out
}

/// Which of `oids` exist as commits in `git`.
pub(crate) fn present_commits(git: &GitRepo, oids: &[String]) -> Result<HashSet<String>> {
    if oids.is_empty() {
        return Ok(HashSet::new());
    }
    let mut child = git.spawn(["cat-file", "--batch-check=%(objectname) %(objecttype)"])?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| ConcurvError::internal("git stdin unavailable"))?;
        for o in oids {
            writeln!(stdin, "{o}").map_err(|e| ConcurvError::io(e, "write to git", git.path()))?;
        }
    }
    let out = finish(child, "cat-file")?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.strip_suffix(" commit"))
        .map(str::to_owned)
        .collect())
}

fn io(e: std::io::Error) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::ExternalTool,
        format!("writing to git fast-import failed: {e}"),
    )
}

/// Export the history of `main` into `git` and point `refs/heads/<branch>`
/// at it. Refuses (`CONCURV_DIVERGED`) if the Git branch has commits that are
/// not part of that history.
pub fn export(repo: &Repo, git: &GitRepo, branch: &str) -> Result<ExportOutcome> {
    export_from(repo, git, "main", branch)
}

/// [`export`] of concurv branch `from` instead of `main` (ADR 0007).
pub fn export_from(repo: &Repo, git: &GitRepo, from: &str, branch: &str) -> Result<ExportOutcome> {
    export_with(
        repo,
        git,
        &ExportOptions {
            from: from.to_owned(),
            branch: branch.to_owned(),
            tags: false,
        },
    )
}

/// Export the history of concurv branch `opts.from` and move Git branch
/// `opts.branch` to it by compare-and-swap; with `opts.tags`, also write the
/// tags that name snapshots of that history, in the same reference
/// transaction (ADR 0008 §6). A Git branch with commits outside the history,
/// or a Git tag of the same name at another commit, refuses the export
/// (`CONCURV_DIVERGED`) and no Git reference changes.
pub fn export_with(repo: &Repo, git: &GitRepo, opts: &ExportOptions) -> Result<ExportOutcome> {
    let (from, branch) = (opts.from.as_str(), opts.branch.as_str());
    let refname = format!("refs/heads/{branch}");
    if !git.run(["check-ref-format", &refname])?.status.success() {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!("`{branch}` is not a valid Git branch name"),
        ));
    }
    let (_, op, _) = repo.current()?;
    let Some(main) = op.view.branches.get(from).copied() else {
        return Err(ConcurvError::new(
            ErrorCode::NotFound,
            format!(
                "concurv has no branch `{from}` (nothing published to it yet?); nothing was exported"
            ),
        ));
    };
    let store = repo.store();
    let graph = ancestors(store, main)?;
    let order = topo_order(&graph, main);

    let mut map = GitMap::load(repo)?;
    let mapped: Vec<String> = order
        .iter()
        .filter_map(|s| map.get(s).map(str::to_owned))
        .collect();
    let present = present_commits(git, &mapped)?;
    let todo: Vec<SnapshotId> = order
        .iter()
        .copied()
        .filter(|s| !map.get(s).is_some_and(|o| present.contains(o)))
        .collect();
    let reused = order.len() - todo.len();

    if !todo.is_empty() {
        let writer = repo.store_writer()?;
        let marks_file = writer.store().tmp_dir().join("git-export-marks");
        let mut child = git.spawn([
            "fast-import".to_owned(),
            "--quiet".to_owned(),
            "--force".to_owned(),
            "--done".to_owned(),
            format!("--export-marks={}", marks_file.display()),
        ])?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ConcurvError::internal("git stdin unavailable"))?;
        let mut w = BufWriter::new(stdin);
        let mut next_mark: u64 = 1;
        let mut blob_marks: HashMap<BlobId, u64> = HashMap::new();
        let mut commit_marks: HashMap<SnapshotId, u64> = HashMap::new();
        let stream = (|| -> Result<()> {
            for s in &todo {
                let snap = store.read_snapshot(s)?;
                let files = repo.snapshot_files(s)?;
                for st in files.values() {
                    if blob_marks.contains_key(&st.blob) {
                        continue;
                    }
                    let (_, len) = store.verify(&st.blob.object_id())?;
                    write!(w, "blob\nmark :{next_mark}\ndata {len}\n").map_err(io)?;
                    store.read_blob_to(&st.blob, &mut w)?;
                    w.write_all(b"\n").map_err(io)?;
                    blob_marks.insert(st.blob, next_mark);
                    next_mark += 1;
                }
                let parent_ref = |p: &SnapshotId| -> Result<String> {
                    if let Some(m) = commit_marks.get(p) {
                        return Ok(format!(":{m}"));
                    }
                    map.get(p)
                        .map(str::to_owned)
                        .ok_or_else(|| ConcurvError::internal("parent exported out of order"))
                };
                if snap.parents.is_empty() {
                    writeln!(w, "reset {EXPORT_REF}").map_err(io)?;
                }
                let (name, email) = git_ident(&snap.author);
                let secs = snap.timestamp_ms.div_euclid(1000).max(0);
                let msg = git_message(&snap.message, &snap.change_id);
                write!(
                    w,
                    "commit {EXPORT_REF}\nmark :{next_mark}\nauthor {name} <{email}> {secs} +0000\n\
                     committer {name} <{email}> {secs} +0000\ndata {}\n{msg}",
                    msg.len()
                )
                .map_err(io)?;
                for (i, p) in snap.parents.iter().enumerate() {
                    let kw = if i == 0 { "from" } else { "merge" };
                    writeln!(w, "{kw} {}", parent_ref(p)?).map_err(io)?;
                }
                writeln!(w, "deleteall").map_err(io)?;
                for (path, st) in &files {
                    let mode = match st.kind {
                        EntryKind::File => "100644",
                        EntryKind::Executable => "100755",
                        EntryKind::Symlink => "120000",
                        EntryKind::Tree => return Err(ConcurvError::internal("tree in file map")),
                    };
                    writeln!(
                        w,
                        "M {mode} :{} {}",
                        blob_marks[&st.blob],
                        quote(&path.to_string())
                    )
                    .map_err(io)?;
                }
                writeln!(w).map_err(io)?;
                commit_marks.insert(*s, next_mark);
                next_mark += 1;
            }
            writeln!(w, "done").map_err(io)?;
            w.flush().map_err(io)
        })();
        drop(w);
        // Always reap the child; its stderr explains a failed stream.
        let finished = finish(child, "fast-import");
        stream?;
        finished?;
        let mut marks = String::new();
        fs::File::open(&marks_file)
            .and_then(|mut f| f.read_to_string(&mut marks))
            .map_err(|e| ConcurvError::io(e, "read fast-import marks", &marks_file))?;
        let by_mark: HashMap<u64, String> = marks
            .lines()
            .filter_map(|l| {
                let (m, oid) = l.strip_prefix(':')?.split_once(' ')?;
                Some((m.parse().ok()?, oid.to_owned()))
            })
            .collect();
        for (s, m) in &commit_marks {
            let oid = by_mark.get(m).ok_or_else(|| {
                ConcurvError::new(
                    ErrorCode::ExternalTool,
                    "git fast-import did not report a commit id",
                )
            })?;
            map.insert(*s, oid.clone());
        }
        let _ = git.run(["update-ref", "-d", EXPORT_REF]);
    }
    let tip = map
        .get(&main)
        .ok_or_else(|| ConcurvError::internal("the branch head was not exported"))?
        .to_owned();
    map.save(repo)?;

    // Compare-and-swap the Git branch; never move it backwards or sideways.
    let previous = git.resolve(&refname)?;
    let move_branch = previous.as_deref() != Some(tip.as_str());
    if move_branch
        && let Some(old) = &previous
        && !git.is_ancestor(old, &tip)?
    {
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "Git branch `{branch}` is at {old}, which is not part of concurv's `{from}` history; \
                 nothing was changed in Git. Integrate those Git commits first (`concurv git import \
                 --into <new branch>`, then `concurv merge`), or export to a new branch"
            ),
        ));
    }

    // Tags: created if missing, kept if equal, refused if different.
    let mut tags = Vec::new();
    let mut tags_skipped = Vec::new();
    let mut conflicts = Vec::new();
    if opts.tags {
        for (name, target) in &op.view.tags {
            let in_history = graph.contains_key(target);
            let Some(commit) = map.get(target).filter(|_| in_history) else {
                tags_skipped.push(name.clone());
                continue;
            };
            let tag_ref = format!("refs/tags/{name}");
            match git.resolve(&tag_ref)? {
                None => tags.push(TagExport {
                    name: name.clone(),
                    commit: commit.to_owned(),
                    created: true,
                }),
                Some(c) if c == commit => tags.push(TagExport {
                    name: name.clone(),
                    commit: commit.to_owned(),
                    created: false,
                }),
                Some(c) => conflicts.push(format!("{name} (Git: {c}, concurv: {commit})")),
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(ConcurvError::new(
            ErrorCode::Diverged,
            format!(
                "Git already has tag(s) with these names at other commits: {}; tags never move, so \
                 nothing was changed in Git (no branch or tag). Rename or delete the tag on one side",
                conflicts.join(", ")
            ),
        ));
    }

    // One transaction for the branch and every new tag: all or nothing.
    let mut commands = String::new();
    if move_branch {
        match &previous {
            Some(old) => commands.push_str(&format!("update {refname} {tip} {old}\n")),
            None => commands.push_str(&format!("create {refname} {tip}\n")),
        }
    }
    for t in tags.iter().filter(|t| t.created) {
        commands.push_str(&format!("create refs/tags/{} {}\n", t.name, t.commit));
    }
    if !commands.is_empty() {
        let mut child = git.spawn(["update-ref", "-m", "concurv git export", "--stdin"])?;
        let wrote = child
            .stdin
            .take()
            .ok_or_else(|| ConcurvError::internal("git stdin unavailable"))
            .and_then(|mut stdin| {
                stdin
                    .write_all(commands.as_bytes())
                    .map_err(|e| ConcurvError::io(e, "write to git", git.path()))
            });
        let finished = finish(child, "update-ref (branch and tags)");
        wrote?;
        finished?;
    }
    Ok(ExportOutcome {
        branch: branch.to_owned(),
        tip,
        previous,
        exported: todo.len(),
        reused,
        updated: move_branch,
        tags,
        tags_skipped,
    })
}
