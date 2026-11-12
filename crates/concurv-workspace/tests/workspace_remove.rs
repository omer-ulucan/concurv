#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Backlog P1-8: `workspace remove`. Refusals (primary, itself, unknown,
//! unpublished saves, unsaved changes, pending merge), files left on disk,
//! a deleted directory, invariant B1, undo refusal.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use concurv_core::ErrorCode;
use concurv_workspace::{
    Repo, UpdateOptions, publish, save, tags, undo, update_with, verify, workspaces,
};

fn setup() -> (tempfile::TempDir, Repo) {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    fs::write(ra.join("f.txt"), "base\n").unwrap();
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    (outer, a)
}

fn ws(outer: &Path, a: &Repo, name: &str) -> Repo {
    let p = outer.join(name);
    workspaces::add(a, &p, None).unwrap();
    Repo::open(&p, opts()).unwrap()
}

fn names(r: &Repo) -> Vec<String> {
    workspaces::list(r)
        .unwrap()
        .into_iter()
        .map(|w| w.name)
        .collect()
}

#[test]
fn refusals_then_removal_leaves_the_files() {
    let (outer, a) = setup();
    let b = ws(outer.path(), &a, "b");
    let code = |name: &str, from: &Repo| workspaces::remove(from, name).unwrap_err().code;
    assert_eq!(code("default", &b), ErrorCode::InvalidArgument, "primary");
    assert_eq!(code("b", &b), ErrorCode::InvalidArgument, "itself");
    assert_eq!(code("nope", &a), ErrorCode::NotFound);

    // Unpublished saves in b: refused (B1).
    fs::write(b.root().join("g.txt"), "work\n").unwrap();
    let s = save(&b, "work").unwrap().snapshot.unwrap();
    assert_eq!(code("b", &a), ErrorCode::Diverged);
    // Kept by a tag: then only the unsaved edit stops it.
    tags::create(&a, "keep/b", Some(s)).unwrap();
    fs::write(b.root().join("g.txt"), "unsaved\n").unwrap();
    assert_eq!(code("b", &a), ErrorCode::DirtyWorkspace);
    assert_eq!(names(&a), ["b", "default"]);

    fs::write(b.root().join("g.txt"), "work\n").unwrap();
    let o = workspaces::remove(&a, "b").unwrap();
    assert!(o.directory_checked);
    assert_eq!(names(&a), ["default"]);
    // The files stay; the directory no longer works as a workspace.
    assert_eq!(
        fs::read_to_string(b.root().join("g.txt")).unwrap(),
        "work\n"
    );
    let e = Repo::open(b.root(), opts()).unwrap().current().unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(e.message.contains("removed"), "{}", e.message);
    // undo refuses; the snapshots are all still reachable.
    let e = undo(&a).unwrap_err();
    assert!(e.message.contains("workspace-remove"), "{}", e.message);
    assert!(verify(&a, true).ok());
}

#[test]
fn a_deleted_directory_and_a_pending_merge() {
    let (outer, a) = setup();
    let c = ws(outer.path(), &a, "c");
    let d = ws(outer.path(), &a, "d");
    // c's directory is gone; its head is main's head: removal works.
    let c_root = c.root().to_path_buf();
    drop(c);
    fs::remove_dir_all(&c_root).unwrap();
    let o = workspaces::remove(&a, "c").unwrap();
    assert!(!o.directory_checked);

    // d has a pending merge: refused until it is finished.
    fs::write(a.root().join("f.txt"), "a side\n").unwrap();
    save(&a, "a side").unwrap();
    publish(&a).unwrap();
    fs::write(d.root().join("f.txt"), "d side\n").unwrap();
    save(&d, "d side").unwrap();
    update_with(
        &d,
        UpdateOptions {
            allow_conflicts: true,
        },
    )
    .unwrap();
    assert_eq!(
        workspaces::remove(&a, "d").unwrap_err().code,
        ErrorCode::DirtyWorkspace,
        "the conflict markers are unsaved changes"
    );
    // With the directory out of reach, the pending merge itself refuses.
    let d_root = d.root().to_path_buf();
    let away = outer.path().join("d-away");
    drop(d);
    fs::rename(&d_root, &away).unwrap();
    assert_eq!(
        workspaces::remove(&a, "d").unwrap_err().code,
        ErrorCode::Conflict
    );
    fs::rename(&away, &d_root).unwrap();
    let d = Repo::open(&d_root, opts()).unwrap();
    concurv_workspace::abort_merge(&d).unwrap();
    // d's own save is still unpublished: B1 refuses.
    assert_eq!(
        workspaces::remove(&a, "d").unwrap_err().code,
        ErrorCode::Diverged
    );
    assert_eq!(names(&a), ["d", "default"]);
    assert!(verify(&a, true).ok());
}
