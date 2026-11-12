#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R6 tags (ADR 0008): create, list, delete, names, `--at` resolution,
//! repository format 4, undo refusal, concurrency, and survival across
//! other operations.

mod common;

use std::fs;
use std::sync::{Arc, Barrier};

use common::*;
use concurv_core::ErrorCode;
use concurv_workspace::branches::{self, CreateOptions};
use concurv_workspace::tags::{self, TagInfo};
use concurv_workspace::{Repo, publish, save, undo, verify, workspaces};

/// A repository in `<outer>/a`; secondary workspaces go next to it.
fn setup() -> (tempfile::TempDir, Repo) {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    (outer, a)
}

fn ws(outer: &tempfile::TempDir, a: &Repo, name: &str) -> Repo {
    let p = outer.path().join(name);
    workspaces::add(a, &p, None).unwrap();
    Repo::open(&p, opts()).unwrap()
}

fn saved(r: &Repo, content: &str) -> concurv_core::SnapshotId {
    fs::write(r.root().join("f.txt"), content).unwrap();
    save(r, content).unwrap().snapshot.unwrap()
}

#[test]
fn create_list_delete_and_resolve() {
    let (_d, r) = new_repo();
    // Nothing to tag yet.
    let e = tags::create(&r, "v0", None).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(r.format_version().unwrap(), 1);

    let s1 = saved(&r, "one\n");
    publish(&r).unwrap();
    let s2 = saved(&r, "two\n");

    // Default: this workspace's head (unpublished saves included).
    let o = tags::create(&r, "v1", None).unwrap();
    assert_eq!(o.tag.target, s2);
    assert!(o.upgraded);
    assert_eq!(r.format_version().unwrap(), 4);
    // --at a branch, a tag, a snapshot prefix.
    let at_main = tags::resolve(&r, "main").unwrap();
    assert_eq!(at_main, s1);
    tags::create(&r, "v0.9", Some(at_main)).unwrap();
    let again = tags::create(&r, "rc/1", Some(tags::resolve(&r, "v1").unwrap())).unwrap();
    assert!(!again.upgraded);
    assert_eq!(again.tag.target, s2);
    let prefix = &s1.to_hex()[..12];
    assert_eq!(tags::resolve(&r, prefix).unwrap(), s1);
    assert_eq!(
        tags::list(&r).unwrap(),
        vec![
            TagInfo {
                name: "rc/1".into(),
                target: s2
            },
            TagInfo {
                name: "v0.9".into(),
                target: s1
            },
            TagInfo {
                name: "v1".into(),
                target: s2
            },
        ]
    );

    // A branch created at a tag.
    let b = branches::create(
        &r,
        "from-tag",
        CreateOptions {
            at: Some(tags::resolve(&r, "v0.9").unwrap()),
            switch: false,
        },
    )
    .unwrap();
    assert_eq!(b.head, s1);
    // A branch and a tag of the same name at different snapshots: ambiguous.
    tags::create(&r, "from-tag", Some(s2)).unwrap();
    assert_eq!(
        tags::resolve(&r, "from-tag").unwrap_err().code,
        ErrorCode::AmbiguousId
    );

    // Tags never move, and names collide like Git refs.
    for (name, code) in [
        ("v1", ErrorCode::InvalidArgument),
        ("V1", ErrorCode::InvalidArgument),
        ("rc", ErrorCode::InvalidArgument),
        ("rc/1/x", ErrorCode::InvalidArgument),
        ("HEAD", ErrorCode::InvalidArgument),
        ("a..b", ErrorCode::InvalidArgument),
        ("-x", ErrorCode::InvalidArgument),
        ("caf\u{e9}", ErrorCode::InvalidArgument),
        ("refs/x", ErrorCode::InvalidArgument),
        ("con", ErrorCode::InvalidArgument),
    ] {
        let e = tags::create(&r, name, Some(s1)).unwrap_err();
        assert_eq!(e.code, code, "{name}");
        assert!(e.message.contains("tag"), "{name}: {}", e.message);
    }
    assert_eq!(tags::list(&r).unwrap().len(), 4);

    // Delete keeps the snapshot and reports where the tag was.
    let d = tags::delete(&r, "v1").unwrap();
    assert_eq!(d.tag.target, s2);
    assert!(tags::list(&r).unwrap().iter().all(|t| t.name != "v1"));
    assert_eq!(
        tags::delete(&r, "v1").unwrap_err().code,
        ErrorCode::NotFound
    );
    tags::create(&r, "v1", Some(d.tag.target)).unwrap();
    assert_eq!(tags::resolve(&r, "v1").unwrap(), s2);
    // Deleting does not downgrade the repository.
    assert_eq!(r.format_version().unwrap(), 4);
    assert!(verify(&r, true).ok());
}

#[test]
fn undo_refuses_tag_operations_and_tags_survive_other_operations() {
    let (outer, r) = setup();
    let s1 = saved(&r, "one\n");
    publish(&r).unwrap();
    tags::create(&r, "v1", None).unwrap();
    let e = undo(&r).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.message.contains("tag-create"), "{}", e.message);
    tags::delete(&r, "v1").unwrap();
    assert!(undo(&r).unwrap_err().message.contains("tag-delete"));
    tags::create(&r, "v1", Some(s1)).unwrap();

    // Other commands, in this and another workspace, keep the tags.
    let other = ws(&outer, &r, "w");
    saved(&other, "two\n");
    publish(&other).unwrap();
    saved(&r, "three\n");
    undo(&r).unwrap();
    assert_eq!(tags::resolve(&other, "v1").unwrap(), s1);
    assert_eq!(tags::list(&r).unwrap().len(), 1);
    // A repository reopened by a fresh handle reads the same tags.
    let again = Repo::open(r.root(), opts()).unwrap();
    assert_eq!(tags::list(&again).unwrap(), tags::list(&r).unwrap());
}

#[test]
fn concurrent_creates_of_one_tag_have_one_winner() {
    let (outer, r) = setup();
    saved(&r, "one\n");
    publish(&r).unwrap();
    let repos: Vec<Repo> = (0..5)
        .map(|i| {
            let w = ws(&outer, &r, &format!("t{i}"));
            saved(&w, &format!("w{i}\n"));
            w
        })
        .collect();
    let names = ["v1", "V1", "v1", "v1/x", "V1/y"];
    let barrier = Arc::new(Barrier::new(repos.len()));
    let results: Vec<bool> = std::thread::scope(|s| {
        let hs: Vec<_> = repos
            .iter()
            .zip(names)
            .map(|(w, n)| {
                let barrier = Arc::clone(&barrier);
                s.spawn(move || {
                    barrier.wait();
                    tags::create(w, n, None).is_ok()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1, "{results:?}");
    assert_eq!(tags::list(&r).unwrap().len(), 1);
    assert_eq!(r.format_version().unwrap(), 4);
    assert!(verify(&r, true).ok());
}

#[test]
fn format_4_is_refused_by_a_build_that_does_not_know_it() {
    // A future format is refused the same way older builds refuse format 4.
    let (d, r) = new_repo();
    saved(&r, "one\n");
    tags::create(&r, "v1", None).unwrap();
    assert_eq!(
        fs::read_to_string(d.path().join(".concurv").join("format")).unwrap(),
        "concurv-repo-format 4\n"
    );
    fs::write(
        d.path().join(".concurv").join("format"),
        "concurv-repo-format 5\n",
    )
    .unwrap();
    let e = Repo::open(d.path(), opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedFormat);
}
