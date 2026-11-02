#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2.5 branches (ADR 0007): naming and namespace rules, create (with
//! `--switch`), list, transactional switch, several workspaces on one branch,
//! per-branch compare-and-swap, `merge <branch>`, and invariant B1 (no
//! acknowledged snapshot becomes unreachable).

mod common;

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

use common::*;
use concurv_core::{ErrorCode, SnapshotId};
use concurv_workspace::branches::{self, CreateOptions};
use concurv_workspace::graph::ancestors;
use concurv_workspace::{
    ChangeKind, Repo, RestoreOptions, UpdateKind, UpdateOptions, merge_branch, publish, restore,
    save, status, undo, update, update_with, verify, workspaces,
};

const DEFAULT: CreateOptions = CreateOptions {
    at: None,
    switch: false,
};

const SWITCH: CreateOptions = CreateOptions {
    at: None,
    switch: true,
};

fn at(s: SnapshotId) -> CreateOptions {
    CreateOptions {
        at: Some(s),
        switch: false,
    }
}

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

/// Invariant B1: every snapshot in `acked` is an ancestor of (or equal to)
/// some current workspace head or branch head.
fn assert_reachable(repo: &Repo, acked: &[SnapshotId]) {
    let (_, op, _) = repo.current().unwrap();
    let mut live: HashSet<SnapshotId> = HashSet::new();
    let tips: Vec<SnapshotId> = op
        .view
        .branches
        .values()
        .copied()
        .chain(op.view.workspaces.values().filter_map(|w| w.head))
        .collect();
    for t in tips {
        live.insert(t);
        live.extend(ancestors(repo.store(), t).unwrap().keys().copied());
    }
    for s in acked {
        assert!(
            live.contains(s),
            "acknowledged snapshot {s} became unreachable"
        );
    }
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

/// A branch `feature` whose head differs from `main` in several ways:
/// a modified file, added files (one in new directories), a deleted file.
fn with_feature(outer: &tempfile::TempDir, a: &Repo) -> Repo {
    branches::create(a, "feature", DEFAULT).unwrap();
    let b = ws(outer, a, "b", Some("feature"));
    set_files(
        b.root(),
        &files(&[
            ("f.txt", b"feature\n"),
            ("new/deep/n.txt", b"n"),
            ("added.txt", b"a"),
        ]),
    );
    save(&b, "feature").unwrap();
    publish(&b).unwrap();
    b
}

#[test]
fn create_and_list() {
    let (_o, a) = setup();
    let base = head_of(&a, "main");
    let out = branches::create(&a, "feature/x", DEFAULT).unwrap();
    assert_eq!(out.head, base);
    assert!(!out.switched);
    let list = branches::list(&a).unwrap();
    let names: Vec<_> = list.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["feature/x", "main"]);
    assert!(list[1].current && !list[0].current);
    assert_eq!(list[1].workspaces, ["default"]);
    assert!(list[0].workspaces.is_empty());
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
    branches::create(&a, "feature/a", DEFAULT).unwrap();
    let before = heads(&a);
    let ops_before = a.oplog().walk(100).unwrap().len();
    for name in [
        "main",
        "MAIN",
        "Main/x",
        "feature",
        "Feature/A",
        "feature/a/b",
        "bad name",
        "a..b",
        "x.lock",
        "-x",
        "HEAD",
        "@",
        "refs/x",
        "remote/x",
        "remotes/o/main",
        "con",
        "a/nul.txt",
        "ü",
        "a<b",
        "a|b",
        "a:b",
        ".hidden",
        "a/",
        "a@{1}",
    ] {
        let e = branches::create(&a, name, DEFAULT).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument, "{name}: {e}");
    }
    let missing = SnapshotId::from_object_id(concurv_core::ObjectId::from_bytes([7; 32]));
    assert!(branches::create(&a, "ghost", at(missing)).is_err());
    // --switch only at this workspace's head.
    fs::write(a.root().join("f.txt"), b"v2\n").unwrap();
    let v2 = save(&a, "v2").unwrap().snapshot.unwrap();
    let base = a.store().read_snapshot(&v2).unwrap().parents[0];
    let e = branches::create(
        &a,
        "elsewhere",
        CreateOptions {
            at: Some(base),
            switch: true,
        },
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(heads(&a), before);
    // Only the save above was recorded.
    assert_eq!(a.oplog().walk(100).unwrap().len(), ops_before + 1);
}

#[test]
fn create_at_an_older_snapshot() {
    let (_o, a) = setup();
    let old = head_of(&a, "main");
    fs::write(a.root().join("f.txt"), b"newer\n").unwrap();
    save(&a, "newer").unwrap();
    publish(&a).unwrap();
    branches::create(&a, "old", at(old)).unwrap();
    assert_eq!(head_of(&a, "old"), old);
}

#[test]
fn create_switch_keeps_unpublished_saves_on_the_new_branch() {
    let (_o, a) = setup();
    let main0 = head_of(&a, "main");
    fs::write(a.root().join("f.txt"), b"one\n").unwrap();
    let s1 = save(&a, "one").unwrap().snapshot.unwrap();
    fs::write(a.root().join("f.txt"), b"two\n").unwrap();
    let s2 = save(&a, "two").unwrap().snapshot.unwrap();
    let before_disk = read_disk(a.root());

    let out = branches::create(&a, "topic", SWITCH).unwrap();
    assert!(out.switched);
    assert_eq!(out.head, s2);
    assert_eq!(tracked(&a).as_deref(), Some("topic"));
    assert_eq!(head_of(&a, "topic"), s2);
    assert_eq!(head_of(&a, "main"), main0, "main untouched");
    assert_eq!(read_disk(a.root()), before_disk, "no file changes");
    assert_reachable(&a, &[main0, s1, s2]);
    // One operation did both; undo refuses it (shared state).
    assert_eq!(undo(&a).unwrap_err().code, ErrorCode::InvalidArgument);
    // Publishing now moves `topic`, and switching back to main is allowed.
    fs::write(a.root().join("f.txt"), b"three\n").unwrap();
    let s3 = save(&a, "three").unwrap().snapshot.unwrap();
    publish(&a).unwrap();
    assert_eq!(head_of(&a, "topic"), s3);
    branches::switch(&a, "main").unwrap();
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"base\n");
    assert_reachable(&a, &[main0, s1, s2, s3]);
}

#[test]
fn publication_is_isolated_per_branch() {
    let (o, a) = setup();
    branches::create(&a, "feature", DEFAULT).unwrap();
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
    // integrated the newest feature head is refused, and nothing moves.
    let c = ws(&o, &a, "c", Some("feature"));
    fs::write(b.root().join("keep"), b"b again").unwrap();
    save(&b, "b2").unwrap();
    publish(&b).unwrap();
    fs::write(c.root().join("new"), b"c").unwrap();
    save(&c, "c").unwrap();
    let before = heads(&a);
    assert_eq!(publish(&c).unwrap_err().code, ErrorCode::Diverged);
    assert_eq!(heads(&a), before);
    // `update` integrates the tracked branch, never `main`.
    let u = update(&c).unwrap();
    assert_eq!(u.branch, "feature");
    assert_eq!(fs::read(c.root().join("keep")).unwrap(), b"b again");
    publish(&c).unwrap();
    assert_eq!(head_of(&a, "main"), ma);
    assert!(verify(&a, true).ok());
}

#[test]
fn several_workspaces_follow_one_branch_without_losing_saves() {
    let (o, a) = setup();
    branches::create(&a, "team", DEFAULT).unwrap();
    let main0 = head_of(&a, "main");
    let team: Vec<Repo> = (0..3)
        .map(|i| ws(&o, &a, &format!("t{i}"), Some("team")))
        .collect();
    let mut acked = Vec::new();
    let mut cas_failures = 0;
    for round in 0..3 {
        for (i, r) in team.iter().enumerate() {
            fs::write(
                r.root().join(format!("t{i}-r{round}.txt")),
                format!("{i}/{round}"),
            )
            .unwrap();
            acked.push(save(r, "work").unwrap().snapshot.unwrap());
        }
        for r in &team {
            loop {
                update(r).unwrap();
                match publish(r) {
                    Ok(_) => break,
                    Err(e) if e.code == ErrorCode::Diverged => cas_failures += 1,
                    Err(e) => panic!("{e}"),
                }
            }
        }
    }
    let _ = cas_failures; // sequential publishes after update rarely conflict
    let tip = head_of(&a, "team");
    let history = ancestors(a.store(), tip).unwrap();
    for s in &acked {
        assert!(
            history.contains_key(s) || *s == tip,
            "{s} missing from team"
        );
    }
    assert_eq!(a.snapshot_files(&tip).unwrap().len(), 2 + 9);
    assert_eq!(head_of(&a, "main"), main0);
    assert_reachable(&a, &acked);
    assert!(verify(&a, true).ok());
}

#[test]
fn stale_publish_to_a_branch_is_a_cas_failure_that_changes_nothing() {
    let (o, a) = setup();
    branches::create(&a, "team", DEFAULT).unwrap();
    let x = ws(&o, &a, "x", Some("team"));
    let y = ws(&o, &a, "y", Some("team"));
    fs::write(x.root().join("x"), b"x").unwrap();
    fs::write(y.root().join("y"), b"y").unwrap();
    let sx = save(&x, "x").unwrap().snapshot.unwrap();
    let sy = save(&y, "y").unwrap().snapshot.unwrap();
    publish(&x).unwrap();
    let before = heads(&a);
    let ops = a.oplog().walk(1000).unwrap().len();
    let e = publish(&y).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert_eq!(heads(&a), before);
    assert_eq!(a.oplog().walk(1000).unwrap().len(), ops);
    assert_eq!(y.head_snapshot().unwrap(), Some(sy));
    update(&y).unwrap();
    publish(&y).unwrap();
    assert_reachable(&a, &[sx, sy]);
}

#[test]
fn merge_another_branch_then_publish_to_own() {
    let (o, a) = setup();
    branches::create(&a, "feature", DEFAULT).unwrap();
    let b = ws(&o, &a, "b", Some("feature"));
    fs::write(b.root().join("feat"), b"F").unwrap();
    save(&b, "feature").unwrap();
    publish(&b).unwrap();
    let feature = head_of(&a, "feature");

    fs::write(a.root().join("mainwork"), b"M").unwrap();
    save(&a, "main work").unwrap();
    let out = merge_branch(&a, "feature", UpdateOptions::default()).unwrap();
    assert_eq!(out.kind, UpdateKind::Merged);
    assert_eq!(out.branch, "feature");
    assert_eq!(fs::read(a.root().join("feat")).unwrap(), b"F");
    assert_eq!(fs::read(a.root().join("mainwork")).unwrap(), b"M");
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    // The merge result is not published until `publish` (CAS).
    assert_ne!(head_of(&a, "main"), a.head_snapshot().unwrap().unwrap());
    let kinds: Vec<_> = a
        .oplog()
        .walk(1)
        .unwrap()
        .into_iter()
        .map(|(_, o)| o.kind)
        .collect();
    assert_eq!(kinds, ["merge"]);
    publish(&a).unwrap();
    let snap = a.store().read_snapshot(&head_of(&a, "main")).unwrap();
    assert!(snap.parents.contains(&feature));
    assert_eq!(head_of(&a, "feature"), feature, "source branch untouched");

    let e = merge_branch(&a, "nope", UpdateOptions::default()).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    let e = merge_branch(&a, "main", UpdateOptions::default()).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "own branch: use update");
    assert_eq!(
        merge_branch(&a, "feature", UpdateOptions::default())
            .unwrap()
            .kind,
        UpdateKind::UpToDate
    );
    // A merge is undoable like an update.
    fs::write(b.root().join("feat"), b"F2").unwrap();
    save(&b, "f2").unwrap();
    publish(&b).unwrap();
    let before = a.head_snapshot().unwrap();
    merge_branch(&a, "feature", UpdateOptions::default()).unwrap();
    assert_eq!(fs::read(a.root().join("feat")).unwrap(), b"F2");
    assert_eq!(undo(&a).unwrap().undone_kind, "merge");
    assert_eq!(a.head_snapshot().unwrap(), before);
    assert_eq!(fs::read(a.root().join("feat")).unwrap(), b"F");
}

#[test]
fn switch_checks_out_the_branch_and_retargets_publish() {
    let (o, a) = setup();
    let _b = with_feature(&o, &a);
    let out = branches::switch(&a, "feature").unwrap();
    assert_eq!(out.from.as_deref(), Some("main"));
    assert!(!out.resumed);
    assert_eq!(tracked(&a).as_deref(), Some("feature"));
    assert_eq!(
        read_disk(a.root()),
        files(&[
            ("f.txt", b"feature\n"),
            ("new/deep/n.txt", b"n"),
            ("added.txt", b"a"),
        ])
    );
    assert_eq!(a.head_snapshot().unwrap(), Some(head_of(&a, "feature")));

    let main0 = head_of(&a, "main");
    fs::write(a.root().join("f.txt"), b"more feature\n").unwrap();
    save(&a, "more").unwrap();
    publish(&a).unwrap();
    assert_eq!(head_of(&a, "main"), main0);
    assert_eq!(head_of(&a, "feature"), a.head_snapshot().unwrap().unwrap());

    assert!(branches::switch(&a, "feature").unwrap().operation.is_none());
    assert_eq!(
        branches::switch(&a, "nope").unwrap_err().code,
        ErrorCode::NotFound
    );
    // Back to main: the new directory is pruned, the deleted file returns.
    branches::switch(&a, "main").unwrap();
    assert_eq!(
        read_disk(a.root()),
        files(&[("f.txt", b"base\n"), ("keep", b"k")])
    );
    assert!(!a.root().join("new").exists());
}

#[test]
fn switch_never_loses_work() {
    let (_o, a) = setup();
    branches::create(&a, "other", DEFAULT).unwrap();
    // Unsaved change to a tracked file: refused, file intact.
    fs::write(a.root().join("f.txt"), b"unsaved\n").unwrap();
    let e = branches::switch(&a, "other").unwrap_err();
    assert_eq!(e.code, ErrorCode::DirtyWorkspace);
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"unsaved\n");
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    // Unsaved deletion of a tracked file: refused too.
    fs::write(a.root().join("f.txt"), b"base\n").unwrap();
    fs::remove_file(a.root().join("keep")).unwrap();
    assert_eq!(
        branches::switch(&a, "other").unwrap_err().code,
        ErrorCode::DirtyWorkspace
    );
    assert!(!a.root().join("keep").exists());
    fs::write(a.root().join("keep"), b"k").unwrap();
    fs::write(a.root().join("f.txt"), b"unsaved\n").unwrap();
    let saved = save(&a, "unpublished").unwrap().snapshot.unwrap();

    // Saved but unpublished: it would be on no branch, so refused.
    let e = branches::switch(&a, "other").unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    assert_eq!(a.head_snapshot().unwrap(), Some(saved));

    // The remedy keeps it on a branch; then switching works.
    branches::create(&a, "keep-this", SWITCH).unwrap();
    branches::switch(&a, "other").unwrap();
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"base\n");
    assert_eq!(head_of(&a, "keep-this"), saved);
    assert_reachable(&a, &[saved]);
    assert!(verify(&a, true).ok());
}

#[test]
fn switch_keeps_untracked_files_and_refuses_collisions() {
    let (o, a) = setup();
    let _b = with_feature(&o, &a);
    let refuse = |paths: &[(&str, &[u8])]| {
        for (p, c) in paths {
            let f = a.root().join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, c).unwrap();
        }
        let before = read_disk(a.root());
        let e = branches::switch(&a, "feature").unwrap_err();
        assert_eq!(e.code, ErrorCode::DirtyWorkspace, "{paths:?}: {e}");
        assert_eq!(read_disk(a.root()), before, "{paths:?}");
        assert_eq!(tracked(&a).as_deref(), Some("main"));
        for (p, _) in paths {
            let f = a.root().join(p);
            fs::remove_file(&f).unwrap();
            if f.parent().unwrap() != a.root() {
                fs::remove_dir(f.parent().unwrap()).unwrap();
            }
        }
    };
    // A different untracked file at a target path.
    refuse(&[("added.txt", b"mine")]);
    // An untracked file where the target needs a directory.
    refuse(&[("new", b"file")]);
    // An untracked file inside a directory the target makes a file.
    refuse(&[("added.txt/inner", b"x")]);

    // Non-colliding untracked files stay; one equal to the target is fine.
    fs::write(a.root().join("notes.txt"), b"my notes").unwrap();
    fs::create_dir_all(a.root().join("new/deep")).unwrap();
    fs::write(a.root().join("new/deep/n.txt"), b"n").unwrap();
    branches::switch(&a, "feature").unwrap();
    assert_eq!(fs::read(a.root().join("notes.txt")).unwrap(), b"my notes");
    let st = status(&a).unwrap();
    let changes: Vec<_> = st
        .changes
        .iter()
        .map(|c| (c.path.to_string(), c.kind))
        .collect();
    assert_eq!(changes, [("notes.txt".to_owned(), ChangeKind::Added)]);
    // And back again, still carried.
    branches::switch(&a, "main").unwrap();
    assert_eq!(fs::read(a.root().join("notes.txt")).unwrap(), b"my notes");
}

#[test]
fn switch_never_overwrites_an_ignored_file() {
    let (o, a) = setup();
    // `feature` tracks `app.env`; on main it is ignored and holds a secret.
    branches::create(&a, "feature", DEFAULT).unwrap();
    let b = ws(&o, &a, "b", Some("feature"));
    fs::write(b.root().join("app.env"), b"FEATURE=1\n").unwrap();
    save(&b, "env").unwrap();
    publish(&b).unwrap();
    fs::write(a.root().join(".gitignore"), b"*.env\n").unwrap();
    save(&a, "ignore env").unwrap();
    publish(&a).unwrap();
    fs::write(a.root().join("app.env"), b"SECRET=hunter2\n").unwrap();
    let e = branches::switch(&a, "feature").unwrap_err();
    assert_eq!(e.code, ErrorCode::DirtyWorkspace, "{e}");
    assert_eq!(
        fs::read(a.root().join("app.env")).unwrap(),
        b"SECRET=hunter2\n"
    );
    assert_eq!(tracked(&a).as_deref(), Some("main"));
}

#[test]
fn switch_is_refused_during_a_pending_merge() {
    let (o, a) = setup();
    branches::create(&a, "other", DEFAULT).unwrap();
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
    assert_eq!(
        branches::create(&b, "x", SWITCH).unwrap_err().code,
        ErrorCode::Conflict
    );
}

#[test]
fn undo_reverses_a_switch_but_not_a_create() {
    let (o, a) = setup();
    let _b = with_feature(&o, &a);
    // The most recent operation of `a` is the branch-create.
    assert_eq!(undo(&a).unwrap_err().code, ErrorCode::InvalidArgument);
    fs::write(a.root().join("notes.txt"), b"carried").unwrap();
    branches::switch(&a, "feature").unwrap();
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"feature\n");
    let u = undo(&a).unwrap();
    assert_eq!(u.undone_kind, "branch-switch");
    assert_eq!(tracked(&a).as_deref(), Some("main"));
    assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"base\n");
    assert!(!a.root().join("added.txt").exists());
    assert_eq!(fs::read(a.root().join("notes.txt")).unwrap(), b"carried");
    assert_eq!(a.head_snapshot().unwrap(), Some(head_of(&a, "main")));
}

/// A switch killed midway leaves a marker and a mix of old and new files.
/// (Simulated by writing what the killed process would have left; the CLI
/// kill test does it with real processes.)
#[test]
fn interrupted_switch_can_be_finished_or_undone() {
    for finish in [true, false] {
        let (o, a) = setup();
        let _b = with_feature(&o, &a);
        let origin = a.head_snapshot().unwrap().unwrap();
        let target = head_of(&a, "feature");
        fs::write(a.root().join("notes.txt"), b"carried").unwrap();
        // Half done: one file already new, an added file written, rest old.
        fs::write(a.root().join("f.txt"), b"feature\n").unwrap();
        fs::write(a.root().join("added.txt"), b"a").unwrap();
        fs::write(
            a.restore_marker(),
            format!(
                "concurv-restore 1\ntarget {target}\nbackup -\norigin {origin}\nbranch feature\n"
            ),
        )
        .unwrap();
        let st = status(&a).unwrap();
        let m = st.interrupted_restore.unwrap();
        assert_eq!(
            (m.origin, m.branch.as_deref()),
            (Some(origin), Some("feature"))
        );
        assert_eq!(tracked(&a).as_deref(), Some("main"), "not committed");

        if finish {
            let out = branches::switch(&a, "feature").unwrap();
            assert!(out.resumed);
            assert_eq!(tracked(&a).as_deref(), Some("feature"));
            assert_eq!(a.head_snapshot().unwrap(), Some(target));
            assert_eq!(fs::read(a.root().join("new/deep/n.txt")).unwrap(), b"n");
            assert!(!a.root().join("keep").exists());
            assert_eq!(fs::read(a.root().join("notes.txt")).unwrap(), b"carried");
        } else {
            let out = restore(&a, origin, RestoreOptions { force: true }).unwrap();
            assert!(out.backup.is_some(), "the mixed copy is kept as a backup");
            assert_eq!(tracked(&a).as_deref(), Some("main"));
            assert_eq!(fs::read(a.root().join("f.txt")).unwrap(), b"base\n");
        }
        assert!(status(&a).unwrap().interrupted_restore.is_none());
        assert!(verify(&a, true).ok());
    }
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

fn race(repos: &[Repo], names: &[&str]) -> Vec<bool> {
    let barrier = Arc::new(Barrier::new(repos.len()));
    std::thread::scope(|s| {
        let handles: Vec<_> = repos
            .iter()
            .zip(names)
            .map(|(r, n)| {
                let barrier = Arc::clone(&barrier);
                s.spawn(move || {
                    barrier.wait();
                    branches::create(r, n, DEFAULT).is_ok()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

#[test]
fn concurrent_creates_of_one_name_have_one_winner() {
    let (o, a) = setup();
    let repos: Vec<Repo> = (0..5).map(|i| ws(&o, &a, &format!("w{i}"), None)).collect();
    let results = race(&repos, &["race"; 5]);
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
    assert!(verify(&a, true).ok());
}

#[test]
fn concurrent_creates_of_colliding_names_have_one_winner() {
    let (o, a) = setup();
    let names = ["topic", "TOPIC", "topic/x", "Topic/y"];
    let repos: Vec<Repo> = (0..names.len())
        .map(|i| ws(&o, &a, &format!("w{i}"), None))
        .collect();
    let results = race(&repos, &names);
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1, "{results:?}");
    let topics = heads(&a)
        .into_iter()
        .filter(|(n, _)| n.to_ascii_lowercase().split('/').next() == Some("topic"))
        .count();
    assert_eq!(topics, 1);
}

#[test]
fn concurrent_publishes_to_different_branches_all_land() {
    let (o, a) = setup();
    for i in 0..4 {
        branches::create(&a, &format!("b{i}"), DEFAULT).unwrap();
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
