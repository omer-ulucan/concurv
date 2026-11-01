#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Deterministic fault injection at every durable-write boundary
//! (R1 gate items 7 and 8). This simulates a process dying at each step of
//! the software write protocol; it is not a power-loss test.

mod common;

use std::fs;

use common::*;
use concurv_objects::{ALL_FAULT_POINTS, FaultMode, FaultPoint, Faults};
use concurv_workspace::{Repo, save, status, verify};

/// Acknowledged state A, then edit to state B (with a new dir, a deletion
/// and a modification) ready to be saved.
fn fixture() -> (tempfile::TempDir, concurv_core::SnapshotId, Files) {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[("a.txt", b"A"), ("keep/k", b"K"), ("del", b"D")]),
    );
    let a = save(&repo, "A").unwrap().snapshot.unwrap();
    let b = files(&[
        ("a.txt", b"A modified"),
        ("keep/k", b"K"),
        ("new/dir/n.bin", &[0, 1, 2, 3, 4, 5, 6, 7]),
    ]);
    set_files(d.path(), &b);
    (d, a, b)
}

fn assert_invariants(
    root: &std::path::Path,
    ack: concurv_core::SnapshotId,
    working: &Files,
    ctx: &str,
) -> Repo {
    let repo = Repo::open(root, opts()).unwrap_or_else(|e| panic!("{ctx}: reopen failed: {e}"));
    // No visible ref points at a missing/corrupt object.
    let rep = verify(&repo, true);
    assert!(rep.ok(), "{ctx}: {:?}", rep.problems);
    // Head is either the acknowledged A or a complete new snapshot whose parent is A.
    let head = repo.head_snapshot().unwrap().unwrap();
    if head != ack {
        let s = repo.store().read_snapshot(&head).unwrap();
        assert_eq!(s.parents, [ack], "{ctx}");
    }
    // A remains reachable and fully readable.
    for st in repo.snapshot_files(&ack).unwrap().values() {
        repo.store().read_blob(&st.blob).unwrap();
    }
    // `save` never modifies working files.
    assert_eq!(read_disk(root), *working, "{ctx}");
    repo
}

/// Upper bound on hits of one fault point during one fixture save; guards
/// against an endless loop if a fault point stopped being reachable.
const MAX_HITS: u64 = 500;

/// For every fault point and mode, fail its 1st, 2nd, 3rd … hit on a fresh
/// fixture until a run completes without reaching the fault. The number of
/// hits is not a constant: random device/workspace/change ids decide which
/// `objects/xx/` fan-out directories already exist, so it is discovered per
/// run instead of being recorded once (which made the test flaky).
#[test]
fn save_survives_failure_at_every_boundary() {
    let mut cases = 0;
    for point in ALL_FAULT_POINTS {
        let modes: &[FaultMode] = if point == FaultPoint::TmpWrite {
            &[FaultMode::Error, FaultMode::TornWrite]
        } else {
            &[FaultMode::Error]
        };
        for &mode in modes {
            for nth in 1.. {
                assert!(
                    nth < MAX_HITS,
                    "{point:?} {mode:?}: fault never stopped firing"
                );
                let ctx = format!("{point:?}#{nth} {mode:?}");
                let (d, a, b) = fixture();
                let faults = Faults::fail_at(point, nth, mode);
                let faulty = Repo::open(d.path(), opts_with(faults.clone())).unwrap();
                let r = save(&faulty, "B");
                drop(faulty);
                if !faults.triggered() {
                    // Fewer than `nth` hits in this run: every boundary of
                    // this point has been covered. The save must have
                    // succeeded completely.
                    let s = r.unwrap_or_else(|e| panic!("{ctx}: failed without a fault: {e}"));
                    assert!(s.snapshot.is_some(), "{ctx}");
                    assert_invariants(d.path(), a, &b, &ctx);
                    assert!(
                        nth > 1 || point == FaultPoint::DirCreate,
                        "{ctx}: point never reached"
                    );
                    break;
                }
                assert!(r.is_err(), "{ctx}: fault fired but save reported success");
                let repo = assert_invariants(d.path(), a, &b, &ctx);
                // Recovery: the next save succeeds and records B exactly.
                let s = save(&repo, "B again").unwrap();
                let head = repo.head_snapshot().unwrap().unwrap();
                assert!(s.snapshot.is_some() || head != a, "{ctx}");
                assert!(status(&repo).unwrap().changes.is_empty(), "{ctx}");
                assert!(verify(&repo, true).ok(), "{ctx}");
                assert_eq!(
                    fs::read_dir(d.path().join(".concurv/tmp")).unwrap().count(),
                    0,
                    "{ctx}: tmp cleaned"
                );
                cases += 1;
            }
        }
    }
    eprintln!("save fault-injection cases exercised: {cases}");
    assert!(cases >= 20, "expected many boundaries, got {cases}");
}

#[test]
fn torn_object_file_after_rename_is_detected_and_healed() {
    // Models a filesystem that persisted a rename but not the data.
    let (d, a, _b) = fixture();
    let repo = Repo::open(d.path(), opts()).unwrap();
    let blob = repo
        .snapshot_files(&a)
        .unwrap()
        .values()
        .next()
        .unwrap()
        .blob;
    let p = repo.store().path_for(&blob.object_id());
    let bytes = fs::read(&p).unwrap();
    fs::write(&p, &bytes[..bytes.len() / 2]).unwrap();
    assert!(!verify(&repo, true).ok());
    // Restoring A must fail closed rather than materialize truncated content.
    let e = concurv_workspace::restore(&repo, a, concurv_workspace::RestoreOptions { force: true })
        .unwrap_err();
    assert_eq!(e.code, concurv_core::ErrorCode::ObjectCorrupt);
}

/// Same discovery approach as the save test: fail each hit of each point
/// until a run no longer reaches the fault.
#[test]
fn init_survives_failure_at_every_boundary() {
    let mut cases = 0;
    for point in ALL_FAULT_POINTS {
        for nth in 1.. {
            assert!(nth < MAX_HITS, "init {point:?}: fault never stopped firing");
            let ctx = format!("init {point:?}#{nth}");
            let d = tempfile::tempdir().unwrap();
            fs::write(d.path().join("user.txt"), b"user data").unwrap();
            let faults = Faults::fail_at(point, nth, FaultMode::Error);
            let r = Repo::init(d.path(), opts_with(faults.clone()));
            if !faults.triggered() {
                let repo = r.unwrap_or_else(|e| panic!("{ctx}: failed without a fault: {e}"));
                assert!(verify(&repo, true).ok(), "{ctx}");
                assert!(nth > 1, "{ctx}: point never reached by init");
                break;
            }
            assert!(r.is_err(), "{ctx}: fault fired but init reported success");
            // Either no repository, or (failure after the final rename) a complete one.
            match Repo::open(d.path(), opts()) {
                Ok(repo) => assert!(verify(&repo, true).ok(), "{ctx}"),
                Err(e) => {
                    assert_eq!(e.code, concurv_core::ErrorCode::NotARepo, "{ctx}: {e}");
                    // A retry works despite staging debris.
                    let repo = Repo::init(d.path(), opts()).unwrap();
                    assert!(verify(&repo, false).ok(), "{ctx}");
                }
            }
            assert_eq!(fs::read(d.path().join("user.txt")).unwrap(), b"user data");
            cases += 1;
        }
    }
    eprintln!("init fault-injection cases exercised: {cases}");
    assert!(cases >= 10);
}
