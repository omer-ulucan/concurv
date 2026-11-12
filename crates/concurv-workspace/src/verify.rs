//! `concurv verify`: read-only integrity check of the whole object graph.

use std::collections::HashSet;
use std::fs;

use concurv_core::{
    BlobId, ConcurvError, ConflictId, ErrorCode, ObjectId, OperationId, SnapshotId, TreeId,
};
use concurv_objects::{EntryKind, ObjectKind};

use crate::repo::Repo;

/// One integrity problem.
#[derive(Clone, Debug)]
pub struct Problem {
    /// Error code classifying the problem.
    pub code: ErrorCode,
    /// Object id or path concerned.
    pub subject: String,
    /// Explanation.
    pub message: String,
}

/// Result of verification.
#[derive(Debug, Default)]
pub struct VerifyReport {
    /// Operations verified.
    pub operations: usize,
    /// Conflict objects (pending merges) verified.
    pub conflicts: usize,
    /// Snapshots verified.
    pub snapshots: usize,
    /// Trees verified.
    pub trees: usize,
    /// Blobs verified.
    pub blobs: usize,
    /// Unreachable objects verified (full mode).
    pub unreachable: usize,
    /// Problems; empty means the repository is intact.
    pub problems: Vec<Problem>,
    /// Informational notes (debris, quarantine) that are not corruption.
    pub notes: Vec<String>,
}

impl VerifyReport {
    /// True when no problems were found.
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }

    fn problem(&mut self, subject: impl ToString, e: &ConcurvError) {
        self.problems.push(Problem {
            code: e.code,
            subject: subject.to_string(),
            message: e.message.clone(),
        });
    }
}

/// Verify everything reachable from the op head; with `full`, also every
/// object file in the store. Never modifies the repository.
pub fn verify(repo: &Repo, full: bool) -> VerifyReport {
    let mut r = VerifyReport::default();
    let store = repo.store();
    let mut seen: HashSet<ObjectId> = HashSet::new();

    let head = match repo.op_head().read() {
        Ok(h) => h,
        Err(e) => {
            r.problem("op_head", &e);
            return r;
        }
    };

    // Operations (all parents, not just first).
    let mut ops: Vec<OperationId> = vec![head];
    let mut snaps: Vec<SnapshotId> = Vec::new();
    let mut conflicts: Vec<ConflictId> = Vec::new();
    while let Some(id) = ops.pop() {
        if !seen.insert(id.object_id()) {
            continue;
        }
        match store.read_operation(&id) {
            Ok(op) => {
                r.operations += 1;
                ops.extend(op.parents.iter().copied());
                snaps.extend(op.view.branches.values().copied());
                snaps.extend(op.view.tags.values().copied());
                snaps.extend(op.view.workspaces.values().filter_map(|w| w.head));
                snaps.extend(
                    op.view
                        .remotes
                        .values()
                        .flat_map(|b| b.values().map(|r| r.head)),
                );
                snaps.extend(op.backup);
                snaps.extend(op.checked_out);
                conflicts.extend(op.view.workspaces.values().filter_map(|w| w.merge));
            }
            Err(e) => r.problem(id, &e),
        }
    }

    // Conflict objects of pending merges, and the snapshots they name.
    while let Some(id) = conflicts.pop() {
        if !seen.insert(id.object_id()) {
            continue;
        }
        match store.read_conflict(&id) {
            Ok(c) => {
                r.conflicts += 1;
                snaps.extend([c.ours, c.theirs, c.worktree]);
                snaps.extend(c.base);
            }
            Err(e) => r.problem(id, &e),
        }
    }

    // Snapshots.
    let mut trees: Vec<(TreeId, bool)> = Vec::new();
    while let Some(id) = snaps.pop() {
        if !seen.insert(id.object_id()) {
            continue;
        }
        match store.read_snapshot(&id) {
            Ok(s) => {
                r.snapshots += 1;
                snaps.extend(s.parents.iter().copied());
                trees.push((s.tree, true));
            }
            Err(e) => r.problem(id, &e),
        }
    }

    // Trees and blobs.
    let mut subtree_refs: HashSet<TreeId> = HashSet::new();
    let mut empty_trees: HashSet<TreeId> = HashSet::new();
    let mut blobs: Vec<BlobId> = Vec::new();
    while let Some((id, is_root)) = trees.pop() {
        if !is_root {
            subtree_refs.insert(id);
        }
        if !seen.insert(id.object_id()) {
            continue;
        }
        match store.read_tree(&id) {
            Ok(t) => {
                r.trees += 1;
                if t.is_empty() {
                    empty_trees.insert(id);
                }
                for e in t.entries() {
                    if e.kind == EntryKind::Tree {
                        trees.push((TreeId::from_object_id(e.id), false));
                    } else {
                        blobs.push(BlobId::from_object_id(e.id));
                    }
                }
            }
            Err(e) => r.problem(id, &e),
        }
    }
    for t in empty_trees.intersection(&subtree_refs) {
        r.problems.push(Problem {
            code: ErrorCode::ObjectCorrupt,
            subject: t.to_string(),
            message: "empty tree referenced as a subdirectory".into(),
        });
    }
    for b in blobs {
        if !seen.insert(b.object_id()) {
            continue;
        }
        match store.verify(&b.object_id()) {
            Ok((ObjectKind::Blob, _)) => r.blobs += 1,
            Ok((k, _)) => r.problems.push(Problem {
                code: ErrorCode::WrongObjectKind,
                subject: b.to_string(),
                message: format!("referenced as a blob but is a {}", k.name()),
            }),
            Err(e) => r.problem(b, &e),
        }
    }

    if full {
        match store.list() {
            Ok(listing) => {
                for id in listing.ids {
                    if seen.contains(&id) {
                        continue;
                    }
                    match store.verify(&id) {
                        Ok(_) => r.unreachable += 1,
                        Err(e) => r.problem(id, &e),
                    }
                }
                for p in listing.stray_files {
                    r.problems.push(Problem {
                        code: ErrorCode::ObjectCorrupt,
                        subject: p.display().to_string(),
                        message: "unexpected file in object directory".into(),
                    });
                }
            }
            Err(e) => r.problem("objects", &e),
        }
        let count = |d: &str| {
            fs::read_dir(repo.store_dir().join(d))
                .map(|rd| rd.count())
                .unwrap_or(0)
        };
        let q = count("quarantine");
        if q > 0 {
            r.notes.push(format!(
                "{q} corrupt object file(s) preserved in .concurv/quarantine"
            ));
        }
        let t = count("tmp");
        if t > 0 {
            r.notes.push(format!(
                "{t} leftover temporary file(s) in .concurv/tmp from an interrupted command (removed by the next save/restore)"
            ));
        }
        if let Ok(rd) = fs::read_dir(repo.root()) {
            for e in rd.flatten() {
                if e.file_name()
                    .to_string_lossy()
                    .starts_with(".concurv-init-")
                {
                    r.notes.push(format!(
                        "stale interrupted-init directory `{}`",
                        e.path().display()
                    ));
                }
            }
        }
    }
    if repo.restore_marker().exists() {
        r.notes
            .push("a restore was interrupted; see `concurv status`".into());
    }
    r
}
