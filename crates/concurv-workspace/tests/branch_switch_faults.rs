#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! A branch switch is transactional (ADR 0007 §3): every injected I/O
//! failure, at every failure point of a switch, leaves either the complete
//! original state (files, head, tracked branch, no marker) or the complete
//! switched state. Untracked files and acknowledged history are never lost.

mod common;

use std::fs;

use common::*;
use concurv_objects::{ALL_FAULT_POINTS, FaultMode, FaultPoint, Faults};
use concurv_workspace::branches::{self, CreateOptions};
use concurv_workspace::{Repo, publish, save, status, verify, workspaces};

const DEFAULT: CreateOptions = CreateOptions {
    at: None,
    switch: false,
};

struct Fixture {
    _outer: tempfile::TempDir,
    root: std::path::PathBuf,
    original: Files,
    switched: Files,
}

fn fixture() -> Fixture {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    set_files(
        &ra,
        &files(&[("f.txt", b"base\n"), ("old/only.txt", b"o"), ("keep", b"k")]),
    );
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    branches::create(&a, "feature", DEFAULT).unwrap();
    let pb = outer.path().join("b");
    workspaces::add_on(&a, &pb, None, Some("feature")).unwrap();
    let b = Repo::open(&pb, opts()).unwrap();
    set_files(
        &pb,
        &files(&[
            ("f.txt", b"feature\n"),
            ("keep", b"k"),
            ("new/deep/n.txt", b"n"),
            ("added.txt", b"a"),
        ]),
    );
    save(&b, "feature").unwrap();
    publish(&b).unwrap();
    fs::write(ra.join("notes.txt"), b"untracked notes").unwrap();
    let original = read_disk(&ra);
    let mut switched = files(&[
        ("f.txt", b"feature\n"),
        ("keep", b"k"),
        ("new/deep/n.txt", b"n"),
        ("added.txt", b"a"),
    ]);
    switched.insert("notes.txt".into(), b"untracked notes".to_vec());
    Fixture {
        _outer: outer,
        root: ra,
        original,
        switched,
    }
}

/// Outcome counts for one fault point.
#[derive(Default)]
struct Counts {
    points: u64,
    rolled_back: u64,
    after_commit: u64,
    resumed: u64,
}

/// Every file is in its original or its switched state, and untracked work
/// is intact.
fn assert_old_or_new(fx: &Fixture, ctx: &str) {
    let disk = read_disk(&fx.root);
    for (p, c) in &disk {
        assert!(
            fx.original.get(p) == Some(c) || fx.switched.get(p) == Some(c),
            "{ctx}: `{p}` holds neither its old nor its new content"
        );
    }
    for p in fx.original.keys().chain(fx.switched.keys()) {
        let in_both = fx.original.contains_key(p) && fx.switched.contains_key(p);
        assert!(!in_both || disk.contains_key(p), "{ctx}: `{p}` vanished");
    }
    assert_eq!(
        disk.get("notes.txt").map(Vec::as_slice),
        Some(&b"untracked notes"[..]),
        "{ctx}: untracked file lost"
    );
}

/// `crash`: after the fault every I/O fails (the process died).
/// Otherwise the fault is transient and the process continues.
fn run_point(point: FaultPoint, mode: FaultMode, crash: bool) -> Counts {
    let mut c = Counts::default();
    for n in 1.. {
        assert!(n < 400, "{point:?}: switch never succeeded");
        let ctx = format!("{point:?}/{mode:?} crash={crash} n={n}");
        let fx = fixture();
        let repo = Repo::open(&fx.root, opts()).unwrap();
        let origin = repo.head_snapshot().unwrap();
        drop(repo);
        let faults = if crash {
            Faults::fail_at(point, n, mode)
        } else {
            Faults::fail_once_at(point, n, mode)
        };
        let faulty = Repo::open(&fx.root, opts_with(faults.clone())).unwrap();
        let result = branches::switch(&faulty, "feature");
        drop(faulty);
        let repo = Repo::open(&fx.root, opts()).unwrap();
        assert!(verify(&repo, true).ok(), "{ctx}");
        let (_, op, ws) = repo.current().unwrap();
        let feature = op.view.branches["feature"];
        match result {
            Ok(_) => {
                assert!(
                    !faults.triggered() || mode == FaultMode::TornWrite,
                    "{ctx}: fault swallowed"
                );
                assert_eq!(read_disk(&fx.root), fx.switched, "{ctx}");
                assert_eq!(ws.branch.as_deref(), Some("feature"), "{ctx}");
                c.points = n - 1;
                return c;
            }
            Err(e) if ws.branch.as_deref() == Some("feature") => {
                // Failed after the commit point (e.g. removing the marker):
                // the switch is complete and consistent.
                c.after_commit += 1;
                assert_eq!(ws.head, Some(feature), "{ctx}: {e}");
                assert_eq!(read_disk(&fx.root), fx.switched, "{ctx}: {e}");
                branches::switch(&repo, "feature").unwrap();
                assert!(
                    status(&repo).unwrap().interrupted_restore.is_none(),
                    "{ctx}"
                );
            }
            Err(e) => {
                assert!(faults.triggered(), "{ctx}: {e}");
                assert_eq!(ws.branch.as_deref(), Some("main"), "{ctx}");
                assert_eq!(ws.head, origin, "{ctx}");
                if crash {
                    // Recovery after a crash: run the same switch again.
                    assert_old_or_new(&fx, &ctx);
                    let out = branches::switch(&repo, "feature").unwrap();
                    if out.resumed {
                        c.resumed += 1;
                    }
                    assert_eq!(read_disk(&fx.root), fx.switched, "{ctx}: after resuming");
                    assert!(
                        status(&repo).unwrap().interrupted_restore.is_none(),
                        "{ctx}"
                    );
                    assert!(verify(&repo, true).ok(), "{ctx}");
                } else {
                    // A transient error is rolled back completely.
                    c.rolled_back += 1;
                    assert_eq!(
                        read_disk(&fx.root),
                        fx.original,
                        "{ctx}: original state not intact after: {e}"
                    );
                    assert!(
                        status(&repo).unwrap().interrupted_restore.is_none(),
                        "{ctx}: marker left behind after a rollback"
                    );
                }
            }
        }
    }
    unreachable!()
}

fn run_all(crash: bool) -> Vec<(String, Counts)> {
    let mut out = Vec::new();
    for point in ALL_FAULT_POINTS {
        let modes: &[FaultMode] = if point == FaultPoint::TmpWrite {
            &[FaultMode::Error, FaultMode::TornWrite]
        } else {
            &[FaultMode::Error]
        };
        for mode in modes {
            out.push((
                format!("{point:?}/{mode:?}"),
                run_point(point, *mode, crash),
            ));
        }
    }
    for (name, c) in &out {
        eprintln!(
            "crash={crash} {name}: {} failure points, {} rolled back, {} resumed, {} after commit",
            c.points, c.rolled_back, c.resumed, c.after_commit
        );
    }
    out
}

#[test]
fn transient_failures_roll_back_to_the_exact_original_state() {
    let all = run_all(false);
    assert!(all.iter().map(|(_, c)| c.rolled_back).sum::<u64>() > 0);
}

#[test]
fn a_crash_at_any_point_is_recovered_by_running_the_switch_again() {
    let all = run_all(true);
    assert!(
        all.iter().map(|(_, c)| c.resumed).sum::<u64>() > 0,
        "some crash point must leave an interrupted switch"
    );
}
