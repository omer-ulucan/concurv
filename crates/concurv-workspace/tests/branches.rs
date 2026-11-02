#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2.5 P1-3: named branches (ADR 0007): create/list/switch, workspaces on a
//! branch, per-branch CAS publication, `update --from`.

mod common;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use common::*;
use concurv_core::{ErrorCode, SnapshotId};
use concurv_workspace::{
    Repo, UpdateKind, UpdateOptions, branches, publish, save, undo, update, update_from,
    update_with, verify, workspaces,
};

fn heads(repo: &Repo) -> Vec<(String, SnapshotId)> {
    repo.current()
        .unwrap()
        .1
        .view
        .branches
        .into_iter()
        .collect()
}

fn head_of(repo: &Repo, b: &str) -> SnapshotId {
    repo.current().unwrap().1.view.branches[b]
}

fn tracked(repo: &Repo) -> Option<String> {
    repo.current().unwrap().2.branch
}

/// Primary workspace `a` with one published snapshot, plus the outer dir.
fn setup() -> (tempfile::TempDir, Repo) {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    set_files(&ra, &files(&[("f.txt", b"base\n"), ("keep", b"k")]));
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    (outer, a)
}

fn ws(outer: &tempfile::TempDir, a: &Repo, name: &str, branch: Option<&str>) -> Repo {
    let p: PathBuf = outer.path().join(name);
    workspaces::add_on(a, &p, None, branch).unwrap();
    Repo::open(&p, opts()).unwrap()
}

#[test]
fn create_and_list() {
    let (_o, a) = setup();
    let base = head_of(&a, "main");
    let out = branches::create(&a, "feature/x", None).unwrap();
    assert_eq!(out.head, base);
    let list = branches::list(&a).unwrap();
    let names: Vec<_> = list.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["feature/x", "main"]);
    assert!(list[1].current && !list[0].current);
    assert_eq!(list[1].workspaces, ["default"]);
    assert!(list[0].workspaces.is_empty());
    // The creating workspace still tracks main.
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    let kinds: Vec<_> = a
        .oplog()
        .walk(1)
        .unwrap()
        .into_iter()
        .map(|(_, o)| o.kind)
        .collect();
    assert_eq!(kinds, ["branch-create"]);
}

#[test]
fn create_refusals_change_nothing() {
    let (_o, a) = setup();
    let before = heads(&a);
    for (name, code) in [
        ("main", ErrorCode::InvalidArgument),
        ("bad name", ErrorCode::InvalidArgument),
        ("a..b", ErrorCode::InvalidArgument),
        ("x.lock", ErrorCode::InvalidArgument),
    ] {
        assert_eq!(
            branches::create(&a, name, None).unwrap_err().code,
            code,
            "{name}"
        );
    }
    let missing = SnapshotId::from_object_id(concurv_core::ObjectId::from_bytes([7; 32]));
    assert!(branches::create(&a, "ghost", Some(missing)).is_err());
    assert_eq!(heads(&a), before);
}

#[test]
fn create_at_an_older_snapshot() {
    let (_o, a) = setup();
    let old = head_of(&a, "main");
    fs::write(a.root().join("f.txt"), b"newer\n").unwrap();
    save(&a, "newer").unwrap();
    publish(&a).unwrap();
    branches::create(&a, "old", Some(old)).unwrap();
    assert_eq!(head_of(&a, "old"), old);
}

#[test]
fn publication_is_isolated_per_branch() {
    let (o, a) = setup();
    branches::create(&a, "feature", None).unwrap();
    let main0 = head_of(&a, "main");
    let b = ws(&o, &a, "b", Some("feature"));
    assert_eq!(tracked(&b).as_deref(), Some("feature"));
    assert_eq!(fs::read(b.root().join("f.txt")).unwrap(), b"base\n");

    fs::write(b.root().join("f.txt"), b"feature work\n").unwrap();
    let fb = save(&b, "feature").unwrap().snapshot.unwrap();
    publish(&b).unwrap();
    assert_eq!(head_of(&a, "feature"), fb);
    assert_eq!(head_of(&a, "main"), main0, "main untouched");

    fs::write(a.root().join("keep"), b"main work").unwrap();
    let ma = save(&a, "main").unwrap().snapshot.unwrap();
    publish(&a).unwrap();
    assert_eq!(head_of(&a, "main"), ma);
    assert_eq!(head_of(&a, "feature"), fb, "feature untouched");

    // CAS is per branch: a second workspace on `feature` that has not
    // integrated `fb` is refused, and nothing moves.
    let c = ws(&o, &a, "c", Some("feature"));
    update(&c).unwrap(); // already at fb (checked out by add)
    fs::write(b.root().join("keep"), b"b again").unwrap();
    save(&b, "b2").unwrap();
    publish(&b).unwrap();
    fs::write(c.root().join("new"), b"c").unwrap();
    save(&c, "c").unwrap();
    let before = heads(&a);
    assert_eq!(publish(&c).unwrap_err().code, ErrorCode::Diverged);
    assert_eq!(heads(&a), before);
    // ... and update on `c` integrates `feature`, never `main`.
    let u = update(&c).unwrap();
    assert_eq!(u.branch, "feature");
    assert_eq!(fs::read(c.root().join("keep")).unwrap(), b"b again");
    publish(&c).unwrap();
    assert_eq!(head_of(&a, "main"), ma);
    assert!(verify(&a, true).ok());
}

#[test]
fn update_from_another_branch_then_publish_to_own() {
    let (o, a) = setup();
    branches::create(&a, "feature", None).unwrap();
    let b = ws(&o, &a, "b", Some("feature"));
    fs::write(b.root().join("feat"), b"F").unwrap();
    save(&b, "feature").unwrap();
    publish(&b).unwrap();
    let feature = head_of(&a, "feature");

    // `a` (on main) has its own work, then merges `feature`.
    fs::write(a.root().join("mainwork"), b"M").unwrap();
    save(&a, "main work").unwrap();
    let out = update_from(&a, "feature", UpdateOptions::default()).unwrap();
    assert_eq!(out.kind, UpdateKind::Merged);
    assert_eq!(out.branch, "feature");
    assert_eq!(fs::read(a.root().join("feat")).unwrap(), b"F");
    assert_eq!(fs::read(a.root().join("mainwork")).unwrap(), b"M");
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    publish(&a).unwrap();
    let snap = a.store().read_snapshot(&head_of(&a, "main")).unwrap();
    assert!(snap.parents.contains(&feature));
    assert_eq!(head_of(&a, "feature"), feature, "source branch untouched");

    assert_eq!(
        update_from(&a, "nope", UpdateOptions::default())
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    // Already merged: up to date.
    assert_eq!(
        update_from(&a, "feature", UpdateOptions::default())
            .unwrap()
            .kind,
        UpdateKind::UpToDate
    );
}

#[test]
fn switch_checks_out_the_branch_and_retargets_publish() {
    let (o, a) = setup();
    branches::create(&a, "feature", None).unwrap();
    let b = ws(&o, &a, "b", Some("feature"));
    fs::write(b.root().join("f.txt"), b"on feature\n").unwrap();
    save(&b, "f").unwrap();
    publish(&b).unwrap();

    let out = branches::switch(&a, "feature").unwrap();
    assert_eq!(out.from.as_deref(), Some("main"));
    assert_eq!(tracked(&a).as_deref(), Some("feature"));
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"on feature\n");
    assert_eq!(a.head_snapshot().unwrap(), Some(head_of(&a, "feature")));

    let main0 = head_of(&a, "main");
    fs::write(a.root().join("f.txt"), b"more feature\n").unwrap();
    save(&a, "more").unwrap();
    publish(&a).unwrap();
    assert_eq!(head_of(&a, "main"), main0);
    assert_eq!(head_of(&a, "feature"), a.head_snapshot().unwrap().unwrap());

    // Switching to the branch already tracked is a no-op.
    assert!(branches::switch(&a, "feature").unwrap().operation.is_none());
    assert_eq!(
        branches::switch(&a, "nope").unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[test]
fn switch_never_loses_work() {
    let (_o, a) = setup();
    branches::create(&a, "other", None).unwrap();
    // Unsaved change: refused, file intact.
    fs::write(a.root().join("f.txt"), b"unsaved\n").unwrap();
    let e = branches::switch(&a, "other").unwrap_err();
    assert_eq!(e.code, ErrorCode::DirtyWorkspace);
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"unsaved\n");
    assert_eq!(tracked(&a).as_deref(), Some("main"));

    // Saved but unpublished: it is on no branch, so refused.
    let saved = save(&a, "unpublished").unwrap().snapshot.unwrap();
    let e = branches::switch(&a, "other").unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    assert_eq!(a.head_snapshot().unwrap(), Some(saved));

    // Keep it on a new branch; then switching is allowed both ways.
    branches::create(&a, "keep", None).unwrap();
    branches::switch(&a, "keep").unwrap();
    assert_eq!(a.head_snapshot().unwrap(), Some(saved));
    branches::switch(&a, "other").unwrap();
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"base\n");
    assert_eq!(head_of(&a, "keep"), saved);
    assert!(verify(&a, true).ok());
}

#[test]
fn switch_is_refused_during_a_pending_merge() {
    let (o, a) = setup();
    branches::create(&a, "other", None).unwrap();
    let b = ws(&o, &a, "b", None);
    fs::write(a.root().join("f.txt"), b"a\n").unwrap();
    save(&a, "a").unwrap();
    publish(&a).unwrap();
    fs::write(b.root().join("f.txt"), b"b\n").unwrap();
    save(&b, "b").unwrap();
    update_with(
        &b,
        UpdateOptions {
            allow_conflicts: true,
        },
    )
    .unwrap();
    assert_eq!(
        branches::switch(&b, "other").unwrap_err().code,
        ErrorCode::Conflict
    );
}

#[test]
fn undo_reverses_a_switch_but_not_a_create() {
    let (_o, a) = setup();
    branches::create(&a, "feature", None).unwrap();
    let e = undo(&a).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.message.contains("branch-create"), "{e}");

    let main_head = a.head_snapshot().unwrap();
    // Give `feature` different content via a snapshot made on main's tree.
    fs::write(a.root().join("f.txt"), b"feature content\n").unwrap();
    let s = save(&a, "for feature").unwrap().snapshot.unwrap();
    branches::create(&a, "feature2", Some(s)).unwrap();
    concurv_workspace::restore(
        &a,
        main_head.unwrap(),
        concurv_workspace::RestoreOptions { force: false },
    )
    .unwrap();
    save(&a, "back to base").unwrap();
    publish(&a).unwrap();

    branches::switch(&a, "feature2").unwrap();
    assert_eq!(
        fs::read(a.root().join("f.txt")).unwrap(),
        b"feature content\n"
    );
    let u = undo(&a).unwrap();
    assert_eq!(u.undone_kind, "branch-switch");
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"base\n");
    assert_eq!(a.head_snapshot().unwrap(), Some(head_of(&a, "main")));
}

#[test]
fn workspace_add_on_a_missing_branch_creates_nothing() {
    let (o, a) = setup();
    let p = o.path().join("x");
    let e = workspaces::add_on(&a, &p, None, Some("nope")).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(!p.exists());
    assert_eq!(workspaces::list(&a).unwrap().len(), 1);
}

#[test]
fn concurrent_creates_of_one_name_have_one_winner() {
    let (o, a) = setup();
    let repos: Vec<Repo> = (0..5).map(|i| ws(&o, &a, &format!("w{i}"), None)).collect();
    let barrier = Arc::new(Barrier::new(repos.len()));
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = repos
            .iter()
            .map(|r| {
                let barrier = Arc::clone(&barrier);
                s.spawn(move || {
                    barrier.wait();
                    branches::create(r, "race", None).is_ok()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
    assert!(verify(&a, true).ok());
}

#[test]
fn concurrent_publishes_to_different_branches_both_land() {
    let (o, a) = setup();
    for i in 0..4 {
        branches::create(&a, &format!("b{i}"), None).unwrap();
    }
    let repos: Vec<Repo> = (0..4)
        .map(|i| {
            let r = ws(&o, &a, &format!("w{i}"), Some(&format!("b{i}")));
            fs::write(r.root().join("f.txt"), format!("work {i}\n")).unwrap();
            save(&r, "work").unwrap();
            r
        })
        .collect();
    let barrier = Arc::new(Barrier::new(repos.len()));
    std::thread::scope(|s| {
        for r in &repos {
            let barrier = Arc::clone(&barrier);
            s.spawn(move || {
                barrier.wait();
                publish(r).unwrap();
            });
        }
    });
    for (i, r) in repos.iter().enumerate() {
        assert_eq!(
            head_of(&a, &format!("b{i}")),
            r.head_snapshot().unwrap().unwrap()
        );
    }
    assert!(verify(&a, true).ok());
}
