#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2 slice 2c: recorded conflicts and their resolution (RFC 0004 §11).

mod common;

use std::fs;
use std::path::Path;

use common::*;
use concurv_core::{ErrorCode, SnapshotId};
use concurv_workspace::{
    ConflictKind, Repo, UpdateKind, UpdateOptions, abort_merge, publish, save, status, undo,
    update, update_with, verify, workspaces,
};

const ALLOW: UpdateOptions = UpdateOptions {
    allow_conflicts: true,
};

/// Two workspaces whose saved work conflicts. A has published; B has saved.
fn diverged(base: &Files, a_files: &Files, b_files: &Files) -> (tempfile::TempDir, Repo, Repo) {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    set_files(&ra, base);
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    workspaces::add(&a, &outer.path().join("b"), None).unwrap();
    let b = Repo::open(&outer.path().join("b"), opts()).unwrap();
    set_files(&ra, a_files);
    save(&a, "a").unwrap();
    publish(&a).unwrap();
    set_files(b.root(), b_files);
    save(&b, "b").unwrap();
    (outer, a, b)
}

fn main_head(repo: &Repo) -> SnapshotId {
    repo.current().unwrap().1.view.branches["main"]
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

#[test]
fn text_conflict_lifecycle_markers_resolution_save_publish() {
    let (_o, a, b) = diverged(
        &files(&[("shared.txt", b"line\n"), ("x", b"x")]),
        &files(&[
            ("shared.txt", b"line from a\n"),
            ("x", b"x"),
            ("a_only", b"a"),
        ]),
        &files(&[("shared.txt", b"line from b\n"), ("x", b"x2")]),
    );
    let w = b.head_snapshot().unwrap().unwrap();
    let m = main_head(&a);

    // Default update still refuses (backward compatible), nothing modified.
    let disk = read_disk(b.root());
    assert_eq!(update(&b).unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(read_disk(b.root()), disk);

    let u = update_with(&b, ALLOW).unwrap();
    assert_eq!(u.kind, UpdateKind::Conflicted);
    assert_eq!(u.head, Some(w), "head does not move during a pending merge");
    let (_, entries) = u.conflict.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, ConflictKind::BothModified);
    assert!(entries[0].markers);
    // Clean parts of the merge are applied; the conflict is written with markers.
    assert_eq!(read(&b.root().join("a_only")), "a");
    assert_eq!(read(&b.root().join("x")), "x2");
    let text = read(&b.root().join("shared.txt"));
    assert!(text.starts_with("<<<<<<< ours"), "{text}");
    assert!(
        text.contains("line from b\n||||||| base")
            && text.contains("=======\nline from a\n>>>>>>> theirs"),
        "{text}"
    );
    let note = b.root().join("shared.txt.concurv-conflict");
    assert!(read(&note).contains("How to resolve"));

    // Status reports the merge; save is refused while unresolved.
    let st = status(&b).unwrap();
    let ms = st.merge.unwrap();
    assert_eq!(ms.theirs, m);
    assert!(!ms.conflicts[0].resolved());
    let op = op_head(&b);
    assert_eq!(save(&b, "too early").unwrap_err().code, ErrorCode::Conflict);
    assert_eq!(op_head(&b), op);
    // Deleting the note but leaving markers is still unresolved.
    fs::remove_file(&note).unwrap();
    assert_eq!(
        save(&b, "markers left").unwrap_err().code,
        ErrorCode::Conflict
    );
    // Another update is refused while the merge is pending.
    assert_eq!(
        update_with(&b, ALLOW).unwrap_err().code,
        ErrorCode::Conflict
    );

    // Resolve and save: a merge snapshot with both parents.
    fs::write(b.root().join("shared.txt"), "line from a and b\n").unwrap();
    assert!(status(&b).unwrap().merge.unwrap().conflicts[0].resolved());
    let s = save(&b, "merge main").unwrap().snapshot.unwrap();
    assert_eq!(b.store().read_snapshot(&s).unwrap().parents, [w, m]);
    assert!(status(&b).unwrap().merge.is_none());
    publish(&b).unwrap();
    let files = b.snapshot_files(&main_head(&a)).unwrap();
    let names: Vec<_> = files.keys().map(|p| p.to_string()).collect();
    assert_eq!(
        names,
        ["a_only", "shared.txt", "x"],
        "no side files reached main"
    );
    assert!(verify(&a, true).ok());
}

#[test]
fn binary_and_delete_modify_conflicts_use_side_files() {
    let (_o, _a, b) = diverged(
        &files(&[("bin", &[0, 1, 2]), ("doc", b"v1")]),
        &files(&[("bin", &[0, 9, 9]), ("doc", b"v2 by a")]),
        &files(&[("bin", &[0, 7, 7])]), // b changed bin and deleted doc
    );
    let u = update_with(&b, ALLOW).unwrap();
    let (_, entries) = u.conflict.unwrap();
    let kinds: Vec<_> = entries
        .iter()
        .map(|e| (e.path.to_string(), e.kind, e.markers))
        .collect();
    assert_eq!(
        kinds,
        [
            ("bin".to_string(), ConflictKind::BothModified, false),
            ("doc".to_string(), ConflictKind::DeleteModify, false),
        ]
    );
    let r = b.root();
    assert_eq!(
        fs::read(r.join("bin")).unwrap(),
        [0, 7, 7],
        "ours kept in place"
    );
    assert_eq!(fs::read(r.join("bin.concurv-theirs")).unwrap(), [0, 9, 9]);
    assert!(!r.join("doc").exists(), "ours deleted it");
    assert_eq!(read(&r.join("doc.concurv-theirs")), "v2 by a");

    // Resolve: keep theirs for doc, ours for bin.
    fs::rename(r.join("doc.concurv-theirs"), r.join("doc")).unwrap();
    for f in ["bin.concurv-theirs", "bin.concurv-conflict"] {
        fs::remove_file(r.join(f)).unwrap();
    }
    assert_eq!(
        save(&b, "partial").unwrap_err().code,
        ErrorCode::Conflict,
        "doc note remains"
    );
    fs::remove_file(r.join("doc.concurv-conflict")).unwrap();
    save(&b, "resolved").unwrap();
    assert_eq!(
        read_disk(r),
        files(&[("bin", &[0, 7, 7]), ("doc", b"v2 by a")])
    );
    publish(&b).unwrap();
}

#[test]
fn abort_restores_head_and_backs_up_edits() {
    let (_o, _a, b) = diverged(
        &files(&[("f", b"0")]),
        &files(&[("f", b"a")]),
        &files(&[("f", b"b")]),
    );
    let before = read_disk(b.root());
    update_with(&b, ALLOW).unwrap();
    fs::write(b.root().join("f"), b"half-resolved edit").unwrap();
    let o = abort_merge(&b).unwrap();
    assert!(
        o.backup.is_some(),
        "edits made during the merge are backed up"
    );
    assert_eq!(read_disk(b.root()), before);
    assert!(status(&b).unwrap().merge.is_none());
    // The backup holds the half-resolved state.
    let bk = b.snapshot_files(&o.backup.unwrap()).unwrap();
    assert!(bk.keys().any(|p| p.to_string() == "f.concurv-conflict"));
    assert_eq!(
        abort_merge(&b).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(verify(&b, true).ok());
}

#[test]
fn undo_reverses_a_conflicted_update_and_a_merge_save() {
    let (_o, _a, b) = diverged(
        &files(&[("f", b"0")]),
        &files(&[("f", b"a")]),
        &files(&[("f", b"b")]),
    );
    let before = read_disk(b.root());
    let w = b.head_snapshot().unwrap();
    update_with(&b, ALLOW).unwrap();
    undo(&b).unwrap();
    assert_eq!(read_disk(b.root()), before);
    assert!(status(&b).unwrap().merge.is_none());
    assert_eq!(b.head_snapshot().unwrap(), w);

    // Merge again, resolve, save, then undo the save: the merge is pending
    // again with the resolved files, and saving again completes it.
    update_with(&b, ALLOW).unwrap();
    fs::write(b.root().join("f"), b"ab").unwrap();
    fs::remove_file(b.root().join("f.concurv-conflict")).unwrap();
    save(&b, "merged").unwrap();
    undo(&b).unwrap();
    let st = status(&b).unwrap();
    assert!(
        st.merge
            .as_ref()
            .unwrap()
            .conflicts
            .iter()
            .all(|c| c.resolved())
    );
    assert_eq!(b.head_snapshot().unwrap(), w);
    let s = save(&b, "merged again").unwrap().snapshot.unwrap();
    assert_eq!(b.store().read_snapshot(&s).unwrap().parents.len(), 2);
    assert!(verify(&b, true).ok());
}

#[test]
fn file_directory_conflicts_are_still_refused() {
    let (_o, _a, b) = diverged(
        &files(&[("keep", b"k")]),
        &files(&[("keep", b"k"), ("p", b"file from a")]),
        &files(&[("keep", b"k"), ("p/nested", b"dir from b")]),
    );
    let disk = read_disk(b.root());
    assert_eq!(
        update_with(&b, ALLOW).unwrap_err().code,
        ErrorCode::Conflict
    );
    assert_eq!(read_disk(b.root()), disk);
    assert!(status(&b).unwrap().merge.is_none());
}

#[test]
fn identical_merge_resolution_still_records_both_parents() {
    // Resolving by keeping exactly "ours" must still create the merge
    // snapshot; otherwise publish would stay diverged forever.
    let (_o, a, b) = diverged(
        &files(&[("f", b"0")]),
        &files(&[("f", b"a")]),
        &files(&[("f", b"b")]),
    );
    update_with(&b, ALLOW).unwrap();
    fs::write(b.root().join("f"), b"b").unwrap();
    fs::remove_file(b.root().join("f.concurv-conflict")).unwrap();
    let s = save(&b, "keep ours")
        .unwrap()
        .snapshot
        .expect("merge snapshot created");
    assert_eq!(b.store().read_snapshot(&s).unwrap().parents.len(), 2);
    publish(&b).unwrap();
    assert_eq!(main_head(&a), s);
}
