#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Backlog P2-8 (ADR 0007 §9): `branch delete` and `branch rename`.
//! Refusals (tracked, unmerged, protected, collisions), invariant B1, the
//! workspaces that follow a rename, undo refusal, and the operation log.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use concurv_core::{ErrorCode, SnapshotId};
use concurv_workspace::branches::{self, CreateOptions};
use concurv_workspace::graph::is_ancestor;
use concurv_workspace::policy::POLICY_FILE;
use concurv_workspace::{Repo, publish, save, tags, undo, verify, workspaces};

const DEFAULT: CreateOptions = CreateOptions {
    at: None,
    switch: false,
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

fn ws(outer: &Path, a: &Repo, name: &str, branch: &str) -> Repo {
    let p = outer.join(name);
    workspaces::add_on(a, &p, None, Some(branch)).unwrap();
    Repo::open(&p, opts()).unwrap()
}

fn saved(r: &Repo, file: &str, content: &str) -> SnapshotId {
    fs::write(r.root().join(file), content).unwrap();
    save(r, content).unwrap().snapshot.unwrap()
}

fn names(r: &Repo) -> Vec<String> {
    r.current().unwrap().1.view.branches.into_keys().collect()
}

/// B1: every snapshot in `acked` is contained in some branch, tag,
/// workspace head or remote-tracking reference.
fn all_reachable(r: &Repo, acked: &[SnapshotId]) {
    let view = r.current().unwrap().1.view;
    let mut roots: Vec<SnapshotId> = view.branches.values().copied().collect();
    roots.extend(view.tags.values().copied());
    roots.extend(view.workspaces.values().filter_map(|w| w.head));
    for s in acked {
        assert!(
            roots
                .iter()
                .any(|r0| is_ancestor(r.store(), *s, *r0).unwrap()),
            "{s} became unreachable"
        );
    }
}

#[test]
fn delete_refuses_tracked_and_unmerged_branches() {
    let (outer, a) = setup();
    branches::create(&a, "feature", DEFAULT).unwrap();
    let b = ws(outer.path(), &a, "b", "feature");
    let s = saved(&b, "g.txt", "feature work\n");
    publish(&b).unwrap();

    // Tracked by workspace b: refused, naming it.
    let e = branches::delete(&a, "feature").unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.message.contains("workspace(s) b track"), "{}", e.message);
    // Unknown branch.
    assert_eq!(
        branches::delete(&a, "nope").unwrap_err().code,
        ErrorCode::NotFound
    );

    // b moves to main; feature's work is now on no other reference.
    branches::switch(&b, "main").unwrap();
    let e = branches::delete(&a, "feature").unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged, "{}", e.message);
    assert!(names(&a).contains(&"feature".to_owned()));

    // Kept by a tag: deletion allowed; the snapshot stays reachable.
    tags::create(&a, "archive/feature", Some(s)).unwrap();
    let o = branches::delete(&a, "feature").unwrap();
    assert_eq!(o.head, s);
    assert_eq!(names(&a), ["main"]);
    all_reachable(&a, &[s]);
    // Recreating it from the printed head restores it exactly.
    branches::create(
        &a,
        "feature",
        CreateOptions {
            at: Some(o.head),
            switch: false,
        },
    )
    .unwrap();
    assert_eq!(a.current().unwrap().1.view.branches["feature"], s);
    assert!(verify(&a, true).ok());
}

#[test]
fn merged_branches_delete_and_undo_refuses() {
    let (outer, a) = setup();
    branches::create(&a, "topic", DEFAULT).unwrap();
    let t = ws(outer.path(), &a, "t", "topic");
    let s = saved(&t, "t.txt", "topic\n");
    publish(&t).unwrap();
    // Merge topic into main (a fast-forward), then main contains it.
    concurv_workspace::merge_branch(&a, "topic", Default::default()).unwrap();
    publish(&a).unwrap();
    branches::switch(&t, "main").unwrap();
    let o = branches::delete(&a, "topic").unwrap();
    assert_eq!(o.head, s);
    all_reachable(&a, &[s]);
    let e = undo(&a).unwrap_err();
    assert!(e.message.contains("branch-delete"), "{}", e.message);
    // The operation log keeps the old head.
    let (_, op) = a.oplog().head().unwrap();
    assert_eq!(op.kind, "branch-delete");
    assert!(op.description.contains(&s.to_hex()), "{}", op.description);
}

#[test]
fn rename_moves_the_name_and_its_workspaces() {
    let (outer, a) = setup();
    branches::create(&a, "feat/x", DEFAULT).unwrap();
    let b = ws(outer.path(), &a, "b", "feat/x");
    let c = ws(outer.path(), &a, "c", "feat/x");
    let s = saved(&b, "b.txt", "b\n");

    let o = branches::rename(&a, "feat/x", "feature/x").unwrap();
    assert_eq!(o.workspaces, ["b", "c"]);
    assert_eq!(names(&a), ["feature/x", "main"]);
    assert_eq!(b.current().unwrap().2.branch.as_deref(), Some("feature/x"));
    // Publishing from a workspace that followed goes to the new name.
    publish(&b).unwrap();
    assert_eq!(a.current().unwrap().1.view.branches["feature/x"], s);
    concurv_workspace::update(&c).unwrap();
    assert_eq!(fs::read_to_string(c.root().join("b.txt")).unwrap(), "b\n");

    // Refusals: unknown, same name, collisions with other branches.
    assert_eq!(
        branches::rename(&a, "gone", "x").unwrap_err().code,
        ErrorCode::NotFound
    );
    assert_eq!(
        branches::rename(&a, "feature/x", "feature/x")
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    for bad in ["main", "MAIN", "main/sub", "HEAD", "a..b", "remote/x"] {
        assert_eq!(
            branches::rename(&a, "feature/x", bad).unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{bad}"
        );
    }
    // A change of case only is allowed (it collides only with itself).
    branches::rename(&a, "feature/x", "Feature/X").unwrap();
    assert_eq!(names(&a), ["Feature/X", "main"]);
    let e = undo(&a).unwrap_err();
    assert!(e.message.contains("branch-rename"), "{}", e.message);
    all_reachable(&a, &[s]);
    assert!(verify(&a, true).ok());
}

#[test]
fn protected_branches_cannot_be_deleted_or_renamed() {
    let (outer, a) = setup();
    fs::write(
        a.root().join(POLICY_FILE),
        r#"{"concurv_policy": 1, "protect": ["main", "release/1"],
            "checks": [{"name": "never run here", "argv": ["true"], "timeout_s": 5}],
            "sandbox": "none"}"#,
    )
    .unwrap();
    save(&a, "policy").unwrap();
    publish(&a).unwrap();
    branches::create(&a, "release/1", DEFAULT).unwrap();
    let other = ws(outer.path(), &a, "o", "release/1");
    branches::switch(&other, "main").unwrap();
    for name in ["main", "release/1"] {
        let e = branches::rename(&a, name, "elsewhere").unwrap_err();
        assert_eq!(e.code, ErrorCode::PolicyFail, "{name}: {}", e.message);
    }
    let e = branches::delete(&a, "release/1").unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail, "{}", e.message);
    assert_eq!(names(&a), ["main", "release/1"]);
}
