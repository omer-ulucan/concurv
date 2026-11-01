#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Safe restoration (R1 gate item 6) and interrupted restore recovery.

mod common;

use std::fs;

use arc_core::{ChangeId, ErrorCode, ObjectId, RandomId, SnapshotId, TreeId};
use arc_objects::encoding::blob_id;
use arc_objects::{EntryKind, Faults, ObjectKind, Snapshot, Tree, TreeEntry};
use arc_workspace::{Repo, RestoreOptions, restore, save, status, verify};
use common::*;

const FORCE: RestoreOptions = RestoreOptions { force: true };
const SAFE: RestoreOptions = RestoreOptions { force: false };

#[test]
fn restore_old_snapshot_keeps_history_forward() {
    let (d, repo) = new_repo();
    let v1 = files(&[("a", b"1"), ("dir/b", b"b1"), ("gone/x", b"x")]);
    set_files(d.path(), &v1);
    let s1 = save(&repo, "v1").unwrap().snapshot.unwrap();
    let v2 = files(&[("a", b"2"), ("dir", b"now a file"), ("new/c", b"c")]);
    set_files(d.path(), &v2);
    let s2 = save(&repo, "v2").unwrap().snapshot.unwrap();

    let out = restore(&repo, s1, SAFE).unwrap();
    assert!(out.backup.is_none());
    assert_eq!(read_disk(d.path()), v1);
    assert!(!d.path().join("new").exists(), "emptied directories pruned");
    // Head did not move backwards; the restored state is a pending change.
    assert_eq!(repo.head_snapshot().unwrap(), Some(s2));
    assert!(!status(&repo).unwrap().changes.is_empty());
    let s3 = save(&repo, "back to v1").unwrap().snapshot.unwrap();
    let st = |s: &SnapshotId| repo.store().read_snapshot(s).unwrap();
    assert_eq!(st(&s3).tree, st(&s1).tree);
    assert_eq!(st(&s3).parents, [s2]);
    let kinds: Vec<_> = repo
        .oplog()
        .walk(10)
        .unwrap()
        .into_iter()
        .map(|(_, o)| o.kind)
        .collect();
    assert_eq!(kinds, ["save", "restore", "save", "save", "init"]);
    assert!(verify(&repo, true).ok());
}

#[test]
fn dirty_workspace_is_never_overwritten_without_force() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("a", b"1")]));
    let s1 = save(&repo, "v1").unwrap().snapshot.unwrap();
    set_files(d.path(), &files(&[("a", b"2")]));
    save(&repo, "v2").unwrap();

    // Unsaved modification + untracked file.
    fs::write(d.path().join("a"), b"precious unsaved edit").unwrap();
    fs::write(d.path().join("untracked"), b"also precious").unwrap();
    let before_disk = read_disk(d.path());
    let before_head = op_head(&repo);
    let e = restore(&repo, s1, SAFE).unwrap_err();
    assert_eq!(e.code, ErrorCode::DirtyWorkspace);
    assert!(e.message.contains("untracked"), "{}", e.message);
    assert_eq!(read_disk(d.path()), before_disk);
    assert_eq!(op_head(&repo), before_head);

    // With --force a durable backup is taken first and is restorable.
    let out = restore(&repo, s1, FORCE).unwrap();
    let backup = out.backup.expect("backup snapshot");
    assert_eq!(read_disk(d.path()), files(&[("a", b"1")]));
    let ops = repo.oplog().walk(1).unwrap();
    assert_eq!(ops[0].1.backup, Some(backup));
    restore(&repo, backup, FORCE).unwrap();
    assert_eq!(read_disk(d.path()), before_disk);
    assert!(verify(&repo, true).ok());
}

fn malicious_tree(repo: &Repo, name: &[u8]) -> SnapshotId {
    let blob = repo.store().write_blob_bytes(b"pwned").unwrap();
    let mut payload = 1u32.to_be_bytes().to_vec();
    payload.push(EntryKind::File as u8);
    payload.extend_from_slice(&(name.len() as u16).to_be_bytes());
    payload.extend_from_slice(name);
    payload.extend_from_slice(blob.object_id().as_bytes());
    let tree = repo
        .store()
        .write_object(ObjectKind::Tree, &payload)
        .unwrap();
    snapshot_of(repo, TreeId::from_object_id(tree))
}

fn snapshot_of(repo: &Repo, tree: TreeId) -> SnapshotId {
    repo.store()
        .write_snapshot(&Snapshot {
            tree,
            parents: vec![],
            change_id: ChangeId::from_random(RandomId::from_bytes([1; 16])),
            timestamp_ms: 0,
            author: "attacker".into(),
            message: "evil".into(),
        })
        .unwrap()
}

#[test]
fn path_traversal_trees_are_rejected_before_any_write() {
    let outer = tempfile::tempdir().unwrap();
    let ws = outer.path().join("ws");
    fs::create_dir(&ws).unwrap();
    let repo = Repo::init(&ws, opts()).unwrap();
    fs::write(ws.join("keep"), b"k").unwrap();
    save(&repo, "base").unwrap();
    for name in [
        &b".."[..],
        b"../escape",
        b"..\\escape",
        b"/abs",
        b".arc",
        b".ARC",
        b"a\0b",
        b"",
        b"\xff\xfe",
    ] {
        let s = malicious_tree(&repo, name);
        let e = restore(&repo, s, FORCE).unwrap_err();
        assert_eq!(e.code, ErrorCode::ObjectCorrupt, "{name:?}: {e}");
        assert_eq!(read_disk(&ws), files(&[("keep", b"k")]), "{name:?}");
    }
    let outside: Vec<_> = fs::read_dir(outer.path()).unwrap().collect();
    assert_eq!(outside.len(), 1, "nothing written outside the workspace");
    assert_eq!(
        fs::read(ws.join(".arc/format")).unwrap(),
        b"arc-repo-format 1\n"
    );
}

#[test]
fn wrong_kind_and_missing_blobs_abort_before_any_write() {
    let (d, repo) = new_repo();
    fs::write(d.path().join("keep"), b"k").unwrap();
    let base = save(&repo, "base").unwrap().snapshot.unwrap();
    let base_tree = repo.store().read_snapshot(&base).unwrap().tree;

    // Entry claims to be a file but points at a tree.
    let t = Tree::new(vec![TreeEntry {
        name: "f".into(),
        kind: EntryKind::File,
        id: base_tree.object_id(),
    }])
    .unwrap();
    let s = snapshot_of(&repo, repo.store().write_tree(&t).unwrap());
    assert_eq!(
        restore(&repo, s, FORCE).unwrap_err().code,
        ErrorCode::WrongObjectKind
    );

    // Entry points at a blob that does not exist; other files untouched.
    let t = Tree::new(vec![
        TreeEntry {
            name: "a".into(),
            kind: EntryKind::File,
            id: blob_id(b"never stored").object_id(),
        },
        TreeEntry {
            name: "keep".into(),
            kind: EntryKind::File,
            id: blob_id(b"changed").object_id(),
        },
    ])
    .unwrap();
    let s = snapshot_of(&repo, repo.store().write_tree(&t).unwrap());
    assert_eq!(
        restore(&repo, s, FORCE).unwrap_err().code,
        ErrorCode::ObjectMissing
    );
    assert_eq!(read_disk(d.path()), files(&[("keep", b"k")]));

    // Unknown snapshot id.
    let ghost = SnapshotId::from_object_id(ObjectId::from_bytes([7; 32]));
    assert_eq!(
        restore(&repo, ghost, FORCE).unwrap_err().code,
        ErrorCode::ObjectMissing
    );
}

#[test]
fn corrupt_blob_in_target_aborts_before_any_write() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("a", b"old a"), ("z", b"old z")]));
    let s1 = save(&repo, "v1").unwrap().snapshot.unwrap();
    set_files(d.path(), &files(&[("a", b"new a"), ("z", b"new z")]));
    save(&repo, "v2").unwrap();
    let blob = blob_id(b"old z");
    fs::write(repo.store().path_for(&blob.object_id()), b"ARC1\x01corrupt").unwrap();
    let e = restore(&repo, s1, SAFE).unwrap_err();
    assert_eq!(e.code, ErrorCode::ObjectCorrupt);
    assert_eq!(
        read_disk(d.path()),
        files(&[("a", b"new a"), ("z", b"new z")]),
        "`a` must not be restored when `z` is corrupt"
    );
}

#[cfg(windows)]
#[test]
fn windows_unsafe_names_are_refused_before_any_write() {
    let (d, repo) = new_repo();
    fs::write(d.path().join("keep"), b"k").unwrap();
    save(&repo, "base").unwrap();
    for name in ["CON", "aux.txt", "a:stream", "trailing.", "ARC~1", "q?"] {
        let s = malicious_tree(&repo, name.as_bytes());
        let e = restore(&repo, s, FORCE).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidPath, "{name}: {e}");
        assert_eq!(read_disk(d.path()), files(&[("keep", b"k")]));
    }
    // Case-fold collision.
    let b = repo.store().write_blob_bytes(b"x").unwrap().object_id();
    let t = Tree::new(vec![
        TreeEntry {
            name: "README".into(),
            kind: EntryKind::File,
            id: b,
        },
        TreeEntry {
            name: "readme".into(),
            kind: EntryKind::File,
            id: b,
        },
    ])
    .unwrap();
    let s = snapshot_of(&repo, repo.store().write_tree(&t).unwrap());
    assert_eq!(
        restore(&repo, s, FORCE).unwrap_err().code,
        ErrorCode::InvalidPath
    );
    assert_eq!(read_disk(d.path()), files(&[("keep", b"k")]));
}

#[cfg(unix)]
#[test]
fn symlinked_directory_is_replaced_not_written_through() {
    use std::os::unix::fs::symlink;
    let outer = tempfile::tempdir().unwrap();
    let ws = outer.path().join("ws");
    let victim = outer.path().join("victim");
    fs::create_dir_all(&ws).unwrap();
    fs::create_dir_all(&victim).unwrap();
    let repo = Repo::init(&ws, opts()).unwrap();
    // Target: real directory `d` containing `f`.
    set_files(&ws, &files(&[("d/f", b"inside")]));
    let target = save(&repo, "dir").unwrap().snapshot.unwrap();
    // Now `d` is a symlink to a directory outside the workspace.
    common::clear(&ws);
    symlink(&victim, ws.join("d")).unwrap();
    save(&repo, "link").unwrap();
    restore(&repo, target, SAFE).unwrap();
    assert!(fs::symlink_metadata(ws.join("d")).unwrap().is_dir());
    assert_eq!(fs::read(ws.join("d/f")).unwrap(), b"inside");
    assert_eq!(
        fs::read_dir(&victim).unwrap().count(),
        0,
        "nothing written through the link"
    );
}

/// Fail a restore at every rename it performs (backup objects, marker, each
/// file, op head). After each simulated crash: the op head is unchanged or
/// advanced to a valid op, verify passes, unsaved data is either untouched or
/// preserved in a recorded backup, and re-running the restore completes it.
#[test]
fn interrupted_restore_is_reported_and_resumable() {
    use arc_objects::{FaultMode, FaultPoint};
    let v1 = files(&[("a", b"1"), ("b", b"1"), ("c", b"1")]);
    let mut saw_marker = false;
    for n in 1.. {
        assert!(n < 100, "restore never succeeded");
        let (d, repo) = new_repo();
        set_files(d.path(), &v1);
        let s1 = save(&repo, "v1").unwrap().snapshot.unwrap();
        set_files(d.path(), &files(&[("a", b"2"), ("b", b"2"), ("c", b"2")]));
        save(&repo, "v2").unwrap();
        fs::write(d.path().join("a"), b"dirty").unwrap();
        let dirty = read_disk(d.path());
        let head_before = op_head(&repo);

        let faulty = Repo::open(
            d.path(),
            opts_with(Faults::fail_at(FaultPoint::Rename, n, FaultMode::Error)),
        )
        .unwrap();
        let result = restore(&faulty, s1, FORCE);
        drop(faulty);
        let repo = Repo::open(d.path(), opts()).unwrap();
        assert!(verify(&repo, true).ok(), "n={n}");
        if result.is_ok() {
            assert_eq!(read_disk(d.path()), v1);
            assert!(
                saw_marker,
                "some crash point must leave an interrupted restore"
            );
            break;
        }
        let st = status(&repo).unwrap();
        match st.interrupted_restore {
            None => {
                // Crashed before the marker: either nothing touched, or the
                // op was committed and only marker removal failed.
                if op_head(&repo) == head_before {
                    assert_eq!(read_disk(d.path()), dirty, "n={n}");
                } else {
                    assert_eq!(read_disk(d.path()), v1, "n={n}");
                }
            }
            Some(marker) => {
                saw_marker = true;
                assert_eq!(marker.target, s1);
                let backup = marker.backup.expect("backup recorded before files changed");
                // Re-running completes it and clears the marker.
                restore(&repo, s1, FORCE).unwrap();
                assert_eq!(read_disk(d.path()), v1, "n={n}");
                assert!(status(&repo).unwrap().interrupted_restore.is_none());
                // The pre-restore unsaved data is recoverable from the backup.
                restore(&repo, backup, FORCE).unwrap();
                assert_eq!(read_disk(d.path()), dirty, "n={n}");
            }
        }
    }
}
