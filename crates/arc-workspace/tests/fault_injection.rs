#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Deterministic fault injection at every durable-write boundary
//! (R1 gate items 7 and 8). This simulates a process dying at each step of
//! the software write protocol; it is not a power-loss test.

mod common;

use std::fs;

use arc_objects::{ALL_FAULT_POINTS, FaultMode, FaultPoint, Faults};
use arc_workspace::{Repo, save, status, verify};
use common::*;

/// Acknowledged state A, then edit to state B (with a new dir, a deletion
/// and a modification) ready to be saved.
fn fixture() -> (tempfile::TempDir, arc_core::SnapshotId, Files) {
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
    ack: arc_core::SnapshotId,
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

#[test]
fn save_survives_failure_at_every_boundary() {
    // Count how often each point is hit by a successful save of the fixture.
    let (d, _a, _) = fixture();
    let rec = Faults::recording();
    save(&Repo::open(d.path(), opts_with(rec.clone())).unwrap(), "B").unwrap();
    let mut cases = 0;
    for point in ALL_FAULT_POINTS {
        let modes: &[FaultMode] = if point == FaultPoint::TmpWrite {
            &[FaultMode::Error, FaultMode::TornWrite]
        } else {
            &[FaultMode::Error]
        };
        for &mode in modes {
            for nth in 1..=rec.hits(point) {
                let ctx = format!("{point:?}#{nth} {mode:?}");
                let (d, a, b) = fixture();
                let faulty =
                    Repo::open(d.path(), opts_with(Faults::fail_at(point, nth, mode))).unwrap();
                let r = save(&faulty, "B");
                assert!(r.is_err(), "{ctx}: fault did not surface");
                drop(faulty);
                let repo = assert_invariants(d.path(), a, &b, &ctx);
                // Recovery: the next save succeeds and records B exactly.
                let s = save(&repo, "B again").unwrap();
                let head = repo.head_snapshot().unwrap().unwrap();
                assert!(s.snapshot.is_some() || head != a, "{ctx}");
                assert!(status(&repo).unwrap().changes.is_empty(), "{ctx}");
                assert!(verify(&repo, true).ok(), "{ctx}");
                assert_eq!(
                    fs::read_dir(d.path().join(".arc/tmp")).unwrap().count(),
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
    let e = arc_workspace::restore(&repo, a, arc_workspace::RestoreOptions { force: true })
        .unwrap_err();
    assert_eq!(e.code, arc_core::ErrorCode::ObjectCorrupt);
}

#[test]
fn init_survives_failure_at_every_boundary() {
    let d = tempfile::tempdir().unwrap();
    let rec = Faults::recording();
    Repo::init(d.path(), opts_with(rec.clone())).unwrap();
    let mut cases = 0;
    for point in ALL_FAULT_POINTS {
        for nth in 1..=rec.hits(point) {
            let ctx = format!("init {point:?}#{nth}");
            let d = tempfile::tempdir().unwrap();
            fs::write(d.path().join("user.txt"), b"user data").unwrap();
            let r = Repo::init(
                d.path(),
                opts_with(Faults::fail_at(point, nth, FaultMode::Error)),
            );
            assert!(r.is_err(), "{ctx}");
            // Either no repository, or (failure after the final rename) a complete one.
            match Repo::open(d.path(), opts()) {
                Ok(repo) => assert!(verify(&repo, true).ok(), "{ctx}"),
                Err(e) => {
                    assert_eq!(e.code, arc_core::ErrorCode::NotARepo, "{ctx}: {e}");
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
