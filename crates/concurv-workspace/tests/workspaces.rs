#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2 slice 2a: private workspaces, publish and update (RFC 0004).

mod common;

use std::fs;
use std::path::Path;

use common::*;
use concurv_core::{ChangeId, ErrorCode, RandomId, SnapshotId};
use concurv_objects::Snapshot;
use concurv_objects::fs::{TempSession, clean_tmp_root};
use concurv_workspace::graph::{MergeBase, merge_base};
use concurv_workspace::{Repo, UpdateKind, publish, save, status, update, verify, workspaces};

/// Primary workspace with `files` saved and published to `main`.
fn published(files_: &Files) -> (tempfile::TempDir, Repo) {
    let outer = tempfile::tempdir().unwrap();
    let root = outer.path().join("primary");
    fs::create_dir(&root).unwrap();
    let repo = Repo::init(&root, opts()).unwrap();
    set_files(&root, files_);
    save(&repo, "base").unwrap();
    publish(&repo).unwrap();
    (outer, repo)
}

fn add(repo: &Repo, outer: &Path, name: &str) -> Repo {
    let p = outer.join(name);
    workspaces::add(repo, &p, None).unwrap();
    Repo::open(&p, opts()).unwrap()
}

fn main_head(repo: &Repo) -> Option<SnapshotId> {
    repo.current().unwrap().1.view.branches.get("main").copied()
}

#[test]
fn new_workspace_checks_out_main_and_is_isolated() {
    let base = files(&[("a.txt", b"A"), ("dir/b", b"B")]);
    let (outer, primary) = published(&base);
    let ws = add(&primary, outer.path(), "agent-1");
    assert_eq!(read_disk(ws.root()), base);
    assert_eq!(ws.head_snapshot().unwrap(), main_head(&primary));
    assert!(status(&ws).unwrap().changes.is_empty());

    // Editing and saving in the new workspace never touches the primary.
    fs::write(ws.root().join("a.txt"), b"changed in agent-1").unwrap();
    fs::write(ws.root().join("new"), b"n").unwrap();
    save(&ws, "agent work").unwrap();
    assert_eq!(read_disk(primary.root()), base);
    assert!(status(&primary).unwrap().changes.is_empty());

    let list = workspaces::list(&primary).unwrap();
    let names: Vec<_> = list.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(names, ["agent-1", "default"]);
    assert!(
        list.iter()
            .any(|w| w.primary && w.current && w.name == "default")
    );
    let a = list.iter().find(|w| w.name == "agent-1").unwrap();
    assert_eq!(
        fs::canonicalize(a.root.as_ref().unwrap()).unwrap(),
        fs::canonicalize(ws.root()).unwrap()
    );
    assert!(verify(&primary, true).ok());
}

#[test]
fn save_does_not_move_main_and_publish_fast_forwards() {
    let (_outer, repo) = published(&files(&[("f", b"1")]));
    let m0 = main_head(&repo).unwrap();
    fs::write(repo.root().join("f"), b"2").unwrap();
    let s = save(&repo, "two").unwrap().snapshot.unwrap();
    assert_eq!(main_head(&repo), Some(m0), "save must not publish");
    let p = publish(&repo).unwrap();
    assert_eq!((p.from, p.to), (Some(m0), s));
    assert_eq!(main_head(&repo), Some(s));
    let again = publish(&repo).unwrap();
    assert!(
        again.operation.is_none(),
        "publishing the same head is a no-op"
    );
}

#[test]
fn publish_without_a_saved_snapshot_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), opts()).unwrap();
    assert_eq!(publish(&repo).unwrap_err().code, ErrorCode::InvalidArgument);
}

#[test]
fn diverged_publish_is_refused_and_changes_nothing() {
    let (outer, a) = published(&files(&[("shared", b"0")]));
    let b = add(&a, outer.path(), "b");
    fs::write(a.root().join("a.txt"), b"from a").unwrap();
    save(&a, "a").unwrap();
    publish(&a).unwrap();
    fs::write(b.root().join("b.txt"), b"from b").unwrap();
    save(&b, "b").unwrap();

    let main_before = main_head(&a);
    let op_before = op_head(&a);
    let e = publish(&b).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert_eq!(main_head(&a), main_before);
    assert_eq!(op_head(&a), op_before);
}

#[test]
fn update_fast_forwards_a_clean_workspace_and_refuses_a_dirty_one() {
    let (outer, a) = published(&files(&[("f", b"1")]));
    let b = add(&a, outer.path(), "b");
    fs::write(a.root().join("f"), b"2").unwrap();
    fs::write(a.root().join("g"), b"new").unwrap();
    save(&a, "two").unwrap();
    publish(&a).unwrap();

    // Dirty: refused, nothing modified.
    fs::write(b.root().join("unsaved"), b"precious").unwrap();
    let before = read_disk(b.root());
    let e = update(&b).unwrap_err();
    assert_eq!(e.code, ErrorCode::DirtyWorkspace);
    assert_eq!(read_disk(b.root()), before);

    fs::remove_file(b.root().join("unsaved")).unwrap();
    let u = update(&b).unwrap();
    assert_eq!(u.kind, UpdateKind::FastForward);
    assert_eq!(read_disk(b.root()), read_disk(a.root()));
    assert_eq!(b.head_snapshot().unwrap(), main_head(&a));
    assert_eq!(update(&b).unwrap().kind, UpdateKind::UpToDate);
}

#[test]
fn update_merges_independent_changes_then_publish_fast_forwards() {
    let (outer, a) = published(&files(&[("shared", b"0"), ("x", b"x")]));
    let b = add(&a, outer.path(), "b");
    fs::write(a.root().join("a.txt"), b"from a").unwrap();
    fs::write(a.root().join("shared"), b"edited by a").unwrap();
    save(&a, "a").unwrap();
    publish(&a).unwrap();
    let m = main_head(&a).unwrap();
    fs::write(b.root().join("b.txt"), b"from b").unwrap();
    fs::remove_file(b.root().join("x")).unwrap();
    let w = save(&b, "b").unwrap().snapshot.unwrap();

    let u = update(&b).unwrap();
    assert_eq!(u.kind, UpdateKind::Merged);
    let merged = u.head.unwrap();
    assert_eq!(b.store().read_snapshot(&merged).unwrap().parents, [w, m]);
    assert_eq!(
        read_disk(b.root()),
        files(&[
            ("a.txt", b"from a"),
            ("b.txt", b"from b"),
            ("shared", b"edited by a")
        ])
    );
    // The merge is private until published; now it is a fast-forward.
    assert_eq!(main_head(&a), Some(m));
    publish(&b).unwrap();
    assert_eq!(main_head(&a), Some(merged));
    assert!(verify(&a, true).ok());
}

#[test]
fn same_file_conflict_is_refused_and_changes_nothing() {
    let (outer, a) = published(&files(&[("shared", b"0"), ("other", b"o")]));
    let b = add(&a, outer.path(), "b");
    fs::write(a.root().join("shared"), b"a's version").unwrap();
    save(&a, "a").unwrap();
    publish(&a).unwrap();
    fs::write(b.root().join("shared"), b"b's version").unwrap();
    save(&b, "b").unwrap();

    let disk = read_disk(b.root());
    let head = b.head_snapshot().unwrap();
    let op = op_head(&a);
    let e = update(&b).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(
        e.message.contains("shared: changed on both sides"),
        "{}",
        e.message
    );
    assert_eq!(read_disk(b.root()), disk);
    assert_eq!(b.head_snapshot().unwrap(), head);
    assert_eq!(op_head(&a), op);
}

#[test]
fn workspace_add_rejects_bad_locations_and_names() {
    let (outer, repo) = published(&files(&[("f", b"1")]));
    // Inside the primary workspace (would be versioned by it).
    let inside = repo.root().join("nested");
    assert_eq!(
        workspaces::add(&repo, &inside, Some("n")).unwrap_err().code,
        ErrorCode::InvalidPath
    );
    // Non-empty directory.
    let full = outer.path().join("full");
    fs::create_dir(&full).unwrap();
    fs::write(full.join("x"), b"x").unwrap();
    assert_eq!(
        workspaces::add(&repo, &full, Some("full"))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(fs::read(full.join("x")).unwrap(), b"x");
    // Invalid and duplicate names.
    for bad in ["", "-x", ".x", "a b", "a/b", &"n".repeat(65)] {
        let p = outer.path().join(format!("p{}", bad.len()));
        assert_eq!(
            workspaces::add(&repo, &p, Some(bad)).unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{bad:?}"
        );
    }
    workspaces::add(&repo, &outer.path().join("one"), Some("dup")).unwrap();
    let e = workspaces::add(&repo, &outer.path().join("two"), Some("dup")).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    // A workspace containing an existing one.
    let around = tempfile::tempdir().unwrap();
    let r2 = Repo::init(&around.path().join("inner").tap_mkdir(), opts()).unwrap();
    assert_eq!(
        workspaces::add(&r2, around.path(), Some("outer"))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument,
        "non-empty (contains the primary)"
    );
    assert!(verify(&repo, true).ok());
}

trait TapMkdir {
    fn tap_mkdir(self) -> Self;
}
impl TapMkdir for std::path::PathBuf {
    fn tap_mkdir(self) -> Self {
        fs::create_dir_all(&self).unwrap();
        self
    }
}

#[test]
fn live_temp_sessions_survive_recovery_and_dead_ones_are_removed() {
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), opts()).unwrap();
    let tmp = repo.store().tmp_dir().to_path_buf();
    let live = TempSession::create(&tmp, &concurv_objects::Faults::none()).unwrap();
    fs::write(live.dir().join("in-progress"), b"partial object").unwrap();
    // Dead session: directory plus an unlocked lock file, and a loose file.
    fs::create_dir(tmp.join("s-dead")).unwrap();
    fs::write(tmp.join("s-dead.lock"), b"").unwrap();
    fs::create_dir(tmp.join("s-orphan")).unwrap();
    fs::write(tmp.join("atomic.loose.tmp"), b"debris").unwrap();

    // Another handle runs recovery (as every commit does).
    let other = Repo::open(d.path(), opts()).unwrap();
    drop(other.lock().unwrap());
    assert!(
        live.dir().join("in-progress").exists(),
        "live session deleted"
    );
    for gone in ["s-dead", "s-dead.lock", "s-orphan", "atomic.loose.tmp"] {
        assert!(!tmp.join(gone).exists(), "{gone} not cleaned");
    }
    drop(live);
    let _guard = repo.lock().unwrap();
    assert_eq!(clean_tmp_root(&tmp).unwrap(), 0);
    assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);
}

#[test]
fn criss_cross_history_has_no_single_merge_base() {
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), opts()).unwrap();
    let store = repo.store();
    let tree = concurv_workspace::worktree::write_trees(store, &Default::default()).unwrap();
    let mut n = 0u8;
    let mut snap = |parents: Vec<SnapshotId>| {
        n += 1;
        store
            .write_snapshot(&Snapshot {
                tree,
                parents,
                change_id: ChangeId::from_random(RandomId::from_bytes([n; 16])),
                timestamp_ms: 0,
                author: "t".into(),
                message: format!("s{n}"),
            })
            .unwrap()
    };
    let root = snap(vec![]);
    let a1 = snap(vec![root]);
    let b1 = snap(vec![root]);
    let x = snap(vec![a1, b1]);
    let y = snap(vec![b1, a1]);
    assert_eq!(merge_base(store, a1, b1).unwrap(), MergeBase::One(root));
    assert_eq!(merge_base(store, x, a1).unwrap(), MergeBase::One(a1));
    let mut both = vec![a1, b1];
    both.sort();
    assert_eq!(merge_base(store, x, y).unwrap(), MergeBase::Ambiguous(both));
    let lone = snap(vec![]);
    assert_eq!(merge_base(store, lone, x).unwrap(), MergeBase::None);
}
