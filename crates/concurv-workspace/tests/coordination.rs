#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R5 (RFC 0006): advisory intents with a TTL, overlap warnings between
//! workspaces and against upstream work, dependency warnings for changed
//! definitions, and the versioned event stream. Nothing here ever refuses
//! work.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use concurv_workspace::coord::{self, Level};
use concurv_workspace::{Repo, RepoOptions, publish, save, workspaces};

fn write(root: &Path, p: &str, c: &str) {
    let f = root.join(p);
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(f, c).unwrap();
}

fn lines(n: usize, tag: &str) -> String {
    (0..n).map(|i| format!("line {i} {tag}\n")).collect()
}

fn with_line(base: &str, idx: usize, new: &str) -> String {
    base.lines()
        .enumerate()
        .map(|(i, l)| {
            if i == idx {
                format!("{new}\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect()
}

/// Primary workspace `a` with `f.txt` published, and a second workspace `b`.
fn two_workspaces() -> (tempfile::TempDir, tempfile::TempDir, Repo, Repo) {
    let (d, a) = new_repo();
    write(d.path(), "f.txt", &lines(40, "base"));
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    let o = tempfile::tempdir().unwrap();
    let outer = o.path().join("ws-b");
    workspaces::add(&a, &outer, Some("b")).unwrap();
    let b = Repo::open(&outer, opts()).unwrap();
    (d, o, a, b)
}

fn levels(r: &Repo) -> Vec<(Level, String, String, Option<String>)> {
    coord::overlaps(r)
        .unwrap()
        .into_iter()
        .map(|o| (o.level, o.source, o.path, o.symbol))
        .collect()
}

#[test]
fn unpublished_work_in_another_workspace_is_reported_at_path_or_conflict_level() {
    let (d, _o, a, b) = two_workspaces();
    let base = lines(40, "base");
    // A saves (does not publish) an edit at line 2.
    write(d.path(), "f.txt", &with_line(&base, 2, "A was here"));
    save(&a, "a").unwrap();
    assert!(levels(&b).is_empty(), "B changed nothing yet");
    // B edits far away (unsaved): same file, no conflict.
    write(b.root(), "f.txt", &with_line(&base, 30, "B was here"));
    assert_eq!(
        levels(&b),
        vec![(
            Level::Path,
            "workspace default".into(),
            "f.txt".into(),
            None
        )]
    );
    // B edits the line next to A's: the merge would conflict.
    write(b.root(), "f.txt", &with_line(&base, 3, "B was here"));
    assert_eq!(levels(&b)[0].0, Level::Conflict);
    // Warnings are advisory: B saves and publishes regardless.
    save(&b, "b").unwrap();
    publish(&b).unwrap();
}

#[test]
fn a_changed_definition_warns_dependent_work() {
    let (d, a) = new_repo();
    write(
        d.path(),
        "src/lib.rs",
        "pub fn parse(x: u32) -> u32 {\n    x\n}\n",
    );
    write(d.path(), "src/app.rs", "fn main() {\n    let _ = 1;\n}\n");
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    let o = tempfile::tempdir().unwrap();
    let wb = o.path().join("ws-b");
    workspaces::add(&a, &wb, Some("b")).unwrap();
    let b = Repo::open(&wb, opts()).unwrap();
    // A changes the signature of `parse` and publishes.
    write(
        d.path(),
        "src/lib.rs",
        "pub fn parse(x: u32, strict: bool) -> u32 {\n    if strict { x } else { 0 }\n}\n",
    );
    save(&a, "parse gets a flag").unwrap();
    publish(&a).unwrap();
    // B, not yet updated, starts calling `parse`: warned.
    write(&wb, "src/app.rs", "fn main() {\n    let _ = parse(1);\n}\n");
    let l = levels(&b);
    assert!(
        l.contains(&(
            Level::Dependency,
            "branch main".into(),
            "src/app.rs".into(),
            Some("parse".into())
        )),
        "{l:?}"
    );
    // A body-only change upstream would not warn: B now updates, and only
    // its own edit remains.
    concurv_workspace::save(&b, "call parse").unwrap();
    concurv_workspace::update(&b).unwrap();
    assert!(
        levels(&b).iter().all(|o| o.0 != Level::Dependency),
        "integrated: no warning"
    );
}

#[test]
fn intents_are_advisory_expire_and_never_lock() {
    let (d, _o, a, b) = two_workspaces();
    let i = coord::declare(
        &a,
        &["src/**".into()],
        &["parse".into()],
        30 * 60_000,
        "refactor",
    )
    .unwrap();
    assert_eq!(coord::local_intents(&b).len(), 1);
    // B touches a matching path, and mentions the symbol elsewhere.
    write(b.root(), "src/x.rs", "fn x() {}\n");
    write(b.root(), "notes.md", "call parse later\n");
    let l = levels(&b);
    let src = format!("intent {} ({})", &i.id[..12], i.who);
    assert!(
        l.contains(&(Level::Intent, src.clone(), "src/x.rs".into(), None)),
        "{l:?}"
    );
    assert!(l.contains(&(Level::Intent, src, "notes.md".into(), Some("parse".into()))));
    // Its own intents never warn A.
    write(d.path(), "src/y.rs", "fn y() {}\n");
    assert!(levels(&a).iter().all(|o| o.0 != Level::Intent));
    // Never a lock: B saves and publishes inside the declared area.
    save(&b, "inside the intent").unwrap();
    publish(&b).unwrap();
    // After the TTL nothing is reported, even by a repository opened later.
    let later = RepoOptions {
        now_ms: Some(1_793_491_200_000 + 31 * 60_000),
        ..opts()
    };
    let b_later = Repo::open(b.root(), later).unwrap();
    assert!(coord::local_intents(&b_later).is_empty());
    write(b.root(), "src/z.rs", "fn z() {}\n");
    assert!(levels(&b_later).iter().all(|o| o.0 != Level::Intent));
    // Release: only the declaring workspace's own intents.
    assert_eq!(coord::release(&b, None).unwrap(), 0);
    assert_eq!(coord::release(&a, Some(&i.id[..8])).unwrap(), 1);
    assert!(coord::local_intents(&a).is_empty());
    // Invalid declarations are refused.
    assert!(coord::declare(&a, &[], &[], 60_000, "").is_err());
    assert!(coord::declare(&a, &[], &["not an identifier".into()], 60_000, "").is_err());
}

#[test]
fn events_are_versioned_json_lines_of_the_operation_log() {
    let (d, a) = new_repo();
    write(d.path(), "f", "1");
    save(&a, "one").unwrap();
    publish(&a).unwrap();
    let ev = coord::events(&a, 100).unwrap();
    let kinds: Vec<&str> = ev.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["init", "save", "publish"]);
    for e in &ev {
        assert_eq!(e["concurv_event"], 1);
        assert_eq!(e["type"], "operation");
        // One line each, no file contents.
        let line = serde_json::to_string(e).unwrap();
        assert!(!line.contains('\n'));
    }
    let publish_ev = &ev[2];
    assert!(publish_ev["branch_moves"]["main"]["from"].is_null());
    assert!(publish_ev["branch_moves"]["main"]["to"].is_string());
    assert_eq!(publish_ev["workspace"], "default");
    assert_eq!(coord::events(&a, 1).unwrap().len(), 1, "the most recent");
}
