#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2 slice 2b: workspace-scoped undo (RFC 0004 §10).

mod common;

use std::fs;

use common::*;
use concurv_core::{ErrorCode, SnapshotId};
use concurv_workspace::{
    Repo, RestoreOptions, publish, restore, save, status, undo, update, verify, workspaces,
};

fn main_head(repo: &Repo) -> Option<SnapshotId> {
    repo.current().unwrap().1.view.branches.get("main").copied()
}

#[test]
fn undo_save_moves_head_back_and_keeps_files() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("f", b"1")]));
    let s1 = save(&repo, "one").unwrap().snapshot.unwrap();
    fs::write(d.path().join("f"), b"2").unwrap();
    let s2 = save(&repo, "two").unwrap().snapshot.unwrap();

    let u = undo(&repo).unwrap();
    assert_eq!((u.undone_kind.as_str(), u.head), ("save", Some(s1)));
    assert!(u.files_restored_to.is_none());
    assert_eq!(repo.head_snapshot().unwrap(), Some(s1));
    assert_eq!(
        read_disk(d.path()),
        files(&[("f", b"2")]),
        "files untouched"
    );
    let st = status(&repo).unwrap();
    assert_eq!(
        st.changes.len(),
        1,
        "the undone save is now an unsaved change"
    );
    // The undone snapshot still exists and is reachable through the op log.
    repo.store().read_snapshot(&s2).unwrap();
    assert!(verify(&repo, true).ok());

    // Undo again goes further back (not redo): before the first save.
    let u = undo(&repo).unwrap();
    assert_eq!(u.head, None);
    assert_eq!(repo.head_snapshot().unwrap(), None);
    assert_eq!(undo(&repo).unwrap_err().code, ErrorCode::NotFound);
    // Saving again works normally.
    assert!(save(&repo, "again").unwrap().snapshot.is_some());
}

#[test]
fn undo_restore_force_brings_back_unsaved_work() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("a", b"old")]));
    let s1 = save(&repo, "v1").unwrap().snapshot.unwrap();
    set_files(d.path(), &files(&[("a", b"new")]));
    save(&repo, "v2").unwrap();
    let unsaved = files(&[("a", b"precious unsaved"), ("extra", b"untracked")]);
    set_files(d.path(), &unsaved);
    restore(&repo, s1, RestoreOptions { force: true }).unwrap();
    assert_eq!(read_disk(d.path()), files(&[("a", b"old")]));

    let u = undo(&repo).unwrap();
    assert_eq!(u.undone_kind, "restore");
    assert_eq!(
        read_disk(d.path()),
        unsaved,
        "unsaved work recovered exactly"
    );
    assert!(verify(&repo, true).ok());
}

#[test]
fn undo_restore_without_backup_returns_to_head_and_refuses_new_edits() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("a", b"1")]));
    let s1 = save(&repo, "v1").unwrap().snapshot.unwrap();
    set_files(d.path(), &files(&[("a", b"2"), ("b", b"b")]));
    save(&repo, "v2").unwrap();
    let head_files = read_disk(d.path());
    restore(&repo, s1, RestoreOptions::default()).unwrap();

    // Edited after the restore: undo must not overwrite that.
    fs::write(d.path().join("a"), b"edited after restore").unwrap();
    let before = read_disk(d.path());
    assert_eq!(undo(&repo).unwrap_err().code, ErrorCode::DirtyWorkspace);
    assert_eq!(read_disk(d.path()), before);

    fs::write(d.path().join("a"), b"1").unwrap();
    undo(&repo).unwrap();
    assert_eq!(read_disk(d.path()), head_files);
    assert!(status(&repo).unwrap().changes.is_empty());
}

#[test]
fn undo_update_restores_files_and_head() {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    set_files(&ra, &files(&[("shared", b"0")]));
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    workspaces::add(&a, &outer.path().join("b"), None).unwrap();
    let b = Repo::open(&outer.path().join("b"), opts()).unwrap();

    fs::write(ra.join("from_a"), b"a").unwrap();
    save(&a, "a").unwrap();
    publish(&a).unwrap();
    fs::write(b.root().join("from_b"), b"b").unwrap();
    let wb = save(&b, "b").unwrap().snapshot.unwrap();
    let before = read_disk(b.root());
    update(&b).unwrap(); // merge
    assert!(b.root().join("from_a").exists());

    let u = undo(&b).unwrap();
    assert_eq!(u.undone_kind, "update");
    assert_eq!(b.head_snapshot().unwrap(), Some(wb));
    assert_eq!(read_disk(b.root()), before);
    // Undo never touched `main` or workspace A.
    assert_eq!(main_head(&a), a.head_snapshot().unwrap());
    // The first checkout of a new workspace cannot be undone.
    let c = outer.path().join("c");
    workspaces::add(&a, &c, None).unwrap();
    let c = Repo::open(&c, opts()).unwrap();
    assert_eq!(undo(&c).unwrap_err().code, ErrorCode::InvalidArgument);
    assert!(verify(&a, true).ok());
}

#[test]
fn undo_refuses_publish_and_never_touches_other_workspaces() {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    set_files(&ra, &files(&[("f", b"1")]));
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    let m = main_head(&a);
    let op = op_head(&a);
    let e = undo(&a).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.message.contains("publish"), "{}", e.message);
    assert_eq!((main_head(&a), op_head(&a)), (m, op), "nothing changed");

    // Interleaved saves in two workspaces: undo in A only reverts A.
    workspaces::add(&a, &outer.path().join("b"), None).unwrap();
    let b = Repo::open(&outer.path().join("b"), opts()).unwrap();
    fs::write(ra.join("f"), b"a2").unwrap();
    let a_before = a.head_snapshot().unwrap();
    save(&a, "a2").unwrap();
    fs::write(b.root().join("g"), b"b2").unwrap();
    let b2 = save(&b, "b2").unwrap().snapshot.unwrap();
    undo(&a).unwrap();
    assert_eq!(a.head_snapshot().unwrap(), a_before);
    assert_eq!(
        b.head_snapshot().unwrap(),
        Some(b2),
        "B's save survives A's undo"
    );
    assert_eq!(main_head(&a), m);
    // B's most recent operation is its own save, regardless of A's ops.
    let u = undo(&b).unwrap();
    assert_eq!(u.undone_kind, "save");
    assert!(verify(&a, true).ok());
}

#[test]
fn undo_operations_are_recorded_with_the_operation_they_reverse() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("f", b"1")]));
    save(&repo, "one").unwrap();
    let saved_op = repo.op_head().read().unwrap();
    let u = undo(&repo).unwrap();
    assert_eq!(u.undone, saved_op);
    let (_, op) = repo.oplog().head().unwrap();
    assert_eq!(op.kind, "undo");
    assert_eq!(op.undoes, Some(saved_op));
}
