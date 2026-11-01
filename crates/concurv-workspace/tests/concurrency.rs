#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2 gate: N simultaneous writers on one logical branch (RFC 0004 §9).
//!
//! Every writer has its own workspace and its own repository handle (separate
//! lock-file handles, so locks behave as between processes). Writers save,
//! then integrate with `update` and `publish`, retrying when another writer
//! moved `main` first ("conflicting target head updates"). Afterwards every
//! acknowledged save must still exist, and `main` must contain every
//! writer's final state.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use common::*;
use concurv_core::{ErrorCode, SnapshotId};
use concurv_workspace::graph::ancestors;
use concurv_workspace::{Repo, publish, save, update, verify, workspaces};

const ROUNDS: usize = 3;

struct WriterLog {
    acked_saves: Vec<SnapshotId>,
    published: Vec<SnapshotId>,
    diverged_retries: usize,
}

fn run_writers(n: usize) {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("primary");
    fs::create_dir(&root).unwrap();
    let primary = Repo::init(&root, opts()).unwrap();
    fs::write(root.join("README"), b"shared base").unwrap();
    save(&primary, "base").unwrap();
    publish(&primary).unwrap();
    let paths: Vec<_> = (0..n)
        .map(|i| {
            let p = outer.path().join(format!("w{i}"));
            workspaces::add(&primary, &p, None).unwrap();
            p
        })
        .collect();

    let barrier = Arc::new(Barrier::new(n));
    let handles: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let p = p.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let repo = Repo::open(&p, opts()).unwrap();
                let mut log = WriterLog {
                    acked_saves: Vec::new(),
                    published: Vec::new(),
                    diverged_retries: 0,
                };
                barrier.wait();
                for round in 0..ROUNDS {
                    // Each writer owns its files, so merges are conflict-free.
                    let dir = p.join(format!("writer-{i}"));
                    fs::create_dir_all(&dir).unwrap();
                    fs::write(
                        dir.join(format!("round-{round}.txt")),
                        format!("w{i} r{round}"),
                    )
                    .unwrap();
                    fs::write(dir.join("latest.txt"), format!("w{i} r{round}")).unwrap();
                    let s = save(&repo, &format!("w{i} round {round}")).unwrap();
                    log.acked_saves.push(s.snapshot.unwrap());
                    loop {
                        update(&repo).unwrap();
                        match publish(&repo) {
                            Ok(o) => {
                                log.published.push(o.to);
                                break;
                            }
                            Err(e) if e.code == ErrorCode::Diverged => log.diverged_retries += 1,
                            Err(e) => panic!("writer {i}: publish failed: {e}"),
                        }
                    }
                }
                log
            })
        })
        .collect();
    let logs: Vec<WriterLog> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // Reopen from scratch (as after a restart) and check invariants.
    let repo = Repo::open(&root, opts()).unwrap();
    let rep = verify(&repo, true);
    assert!(rep.ok(), "n={n}: {:?}", rep.problems);
    let main = repo.current().unwrap().1.view.branches["main"];
    let history = ancestors(repo.store(), main).unwrap();
    for (i, log) in logs.iter().enumerate() {
        for s in &log.acked_saves {
            repo.store().read_snapshot(s).unwrap();
            assert!(
                history.contains_key(s),
                "n={n}: writer {i} save {s} not in main's history"
            );
        }
        for s in &log.published {
            assert!(
                history.contains_key(s),
                "n={n}: writer {i} publish {s} lost"
            );
        }
    }
    // main's tree contains every writer's every round.
    let files = repo.snapshot_files(&main).unwrap();
    for i in 0..n {
        for round in 0..ROUNDS {
            let p =
                concurv_core::RepoPath::parse(&format!("writer-{i}/round-{round}.txt")).unwrap();
            assert!(files.contains_key(&p), "n={n}: {p} missing from main");
        }
        let latest = concurv_core::RepoPath::parse(&format!("writer-{i}/latest.txt")).unwrap();
        let blob = repo.store().read_blob(&files[&latest].blob).unwrap();
        assert_eq!(
            blob,
            format!("w{i} r{}", ROUNDS - 1).as_bytes(),
            "n={n}: stale content"
        );
    }
    // The op log is one linear chain (commits were serialized).
    let ops = repo.oplog().walk(usize::MAX).unwrap();
    assert!(ops.iter().all(|(_, o)| o.parents.len() <= 1));
    let ids: BTreeSet<_> = ops.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids.len(), ops.len());
    // Every acknowledged operation of each writer is present exactly once.
    let saves = ops.iter().filter(|(_, o)| o.kind == "save").count();
    assert_eq!(saves, 1 + n * ROUNDS, "n={n}");
    let retries: usize = logs.iter().map(|l| l.diverged_retries).sum();
    eprintln!(
        "n={n}: {} ops, {retries} diverged-publish retries",
        ops.len()
    );
}

#[test]
fn two_writers() {
    run_writers(2);
}

#[test]
fn five_writers() {
    run_writers(5);
}

#[test]
fn ten_writers() {
    run_writers(10);
}
