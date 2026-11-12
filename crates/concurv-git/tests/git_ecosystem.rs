#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R6 Git ecosystem (ADR 0002, ADR 0008): tags exported and imported, and
//! import into existing history (incremental round trips, divergence,
//! `--into` another branch). Requires `git` on PATH.

use std::fs;
use std::path::Path;
use std::process::Command;

use concurv_core::{ErrorCode, SnapshotId};
use concurv_git::{ExportOptions, GitRepo, ImportOptions, export_with, import_with};
use concurv_workspace::{
    Repo, RepoOptions, UpdateOptions, merge_branch, publish, save, tags, verify,
};

fn opts() -> RepoOptions {
    RepoOptions {
        now_ms: Some(1_793_491_200_123),
        author: Some("Ada Lovelace <ada@example.com>".into()),
        ..RepoOptions::default()
    }
}

/// `git` with a hermetic environment: no user config, fixed identity and dates.
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.autocrlf=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_NAME", "Grace")
        .env("GIT_AUTHOR_EMAIL", "grace@example.com")
        .env("GIT_COMMITTER_NAME", "Grace")
        .env("GIT_COMMITTER_EMAIL", "grace@example.com")
        .env("GIT_AUTHOR_DATE", "1700000000 +0200")
        .env("GIT_COMMITTER_DATE", "1700000000 +0200")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn write(root: &Path, files: &[(&str, &[u8])]) {
    for (p, c) in files {
        let f = root.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, c).unwrap();
    }
}

fn commit(dir: &Path, files: &[(&str, &[u8])], msg: &str) -> String {
    write(dir, files);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn head_of(repo: &Repo, b: &str) -> Option<SnapshotId> {
    repo.current().unwrap().1.view.branches.get(b).copied()
}

fn export_opts(tags: bool) -> ExportOptions {
    ExportOptions {
        from: "main".into(),
        branch: "main".into(),
        tags,
    }
}

fn import_opts(into: &str, tags: bool) -> ImportOptions {
    ImportOptions {
        branch: "main".into(),
        into: into.into(),
        tags,
    }
}

fn new_concurv(dir: &Path) -> Repo {
    fs::create_dir_all(dir).unwrap();
    Repo::init(dir, opts()).unwrap()
}

#[test]
fn tags_are_exported_in_one_transaction_with_the_branch() {
    let outer = tempfile::tempdir().unwrap();
    let a = new_concurv(&outer.path().join("a"));
    write(a.root(), &[("f.txt", b"1\n")]);
    let s1 = save(&a, "one").unwrap().snapshot.unwrap();
    publish(&a).unwrap();
    tags::create(&a, "v1.0", None).unwrap();
    write(a.root(), &[("f.txt", b"2\n")]);
    save(&a, "two").unwrap();
    publish(&a).unwrap();
    tags::create(&a, "release/v2", None).unwrap();
    // A tag outside main's history is listed, not exported.
    write(a.root(), &[("f.txt", b"unpublished\n")]);
    save(&a, "three").unwrap();
    tags::create(&a, "wip", None).unwrap();

    let g = outer.path().join("mirror.git");
    fs::create_dir(&g).unwrap();
    git(&g, &["init", "-q", "--bare", "-b", "main"]);
    let gr = GitRepo::open(&g).unwrap();

    // Without --tags, no tag is written.
    let o = export_with(&a, &gr, &export_opts(false)).unwrap();
    assert!(o.tags.is_empty());
    assert_eq!(git(&g, &["tag", "--list"]), "");

    let o = export_with(&a, &gr, &export_opts(true)).unwrap();
    assert!(!o.updated, "the branch was already exported");
    let created: Vec<&str> = o.tags.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(created, ["release/v2", "v1.0"]);
    assert!(o.tags.iter().all(|t| t.created));
    assert_eq!(o.tags_skipped, ["wip"]);
    assert_eq!(git(&g, &["tag", "--list"]), "release/v2\nv1.0");
    let v1 = git(&g, &["rev-parse", "v1.0^{commit}"]);
    assert_eq!(git(&g, &["log", "-1", "--format=%s", &v1]), "one");
    // Lightweight tags.
    assert_eq!(git(&g, &["cat-file", "-t", "refs/tags/v1.0"]), "commit");

    // Exporting again changes nothing.
    let o = export_with(&a, &gr, &export_opts(true)).unwrap();
    assert!(o.tags.iter().all(|t| !t.created));

    // A Git tag with the same name elsewhere refuses the whole export: the
    // branch does not move either.
    tags::delete(&a, "v1.0").unwrap();
    tags::create(&a, "v1.0", Some(head_of(&a, "main").unwrap())).unwrap();
    publish(&a).unwrap();
    let before = git(&g, &["rev-parse", "main"]);
    let e = export_with(&a, &gr, &export_opts(true)).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert!(e.message.contains("v1.0"), "{}", e.message);
    assert_eq!(git(&g, &["rev-parse", "main"]), before);
    assert_eq!(git(&g, &["rev-parse", "v1.0^{commit}"]), v1);
    let _ = s1;
}

#[test]
fn import_into_existing_history_round_trips_incrementally() {
    let outer = tempfile::tempdir().unwrap();
    // Git side: a repository with history and tags.
    let src = outer.path().join("src");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    let c1 = commit(&src, &[("a.txt", b"one\n")], "first");
    git(&src, &["tag", "v1"]);
    git(&src, &["tag", "-a", "-m", "annotated", "v1-annotated"]);
    commit(&src, &[("a.txt", b"two\n")], "second");
    git(&src, &["checkout", "-q", "-b", "other"]);
    commit(&src, &[("o.txt", b"o\n")], "other work");
    git(&src, &["tag", "outside"]);
    git(&src, &["checkout", "-q", "main"]);
    let gsrc = GitRepo::open(&src).unwrap();

    // First import (fresh repository), with tags.
    let c = new_concurv(&outer.path().join("c"));
    let i = import_with(&c, &gsrc, &import_opts("main", true)).unwrap();
    assert_eq!(i.commits, 2);
    assert!(i.published && i.checked_out);
    let created: Vec<&str> = i.tags_created.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(created, ["v1", "v1-annotated"]);
    assert_eq!(i.annotated_dropped, 1);
    assert_eq!(i.tags_skipped.len(), 1);
    assert_eq!(i.tags_skipped[0].0, "outside");
    assert_eq!(
        tags::resolve(&c, "v1").unwrap(),
        tags::resolve(&c, "v1-annotated").unwrap()
    );
    assert_eq!(c.format_version().unwrap(), 4);
    let first_tip = head_of(&c, "main").unwrap();

    // Importing again: nothing new.
    let i = import_with(&c, &gsrc, &import_opts("main", true)).unwrap();
    assert_eq!(i.commits, 0);
    assert!(!i.published);
    assert!(i.tags_created.is_empty());
    assert_eq!(i.tags_unchanged, 2);

    // New Git commits are imported on top: only the new ones, as a
    // fast-forward of concurv's main, checked out in the clean workspace.
    let c3 = commit(&src, &[("b.txt", b"b\n")], "third");
    let i = import_with(&c, &gsrc, &import_opts("main", false)).unwrap();
    assert_eq!(i.commits, 1);
    assert!(i.published && i.checked_out);
    let tip = c
        .store()
        .read_snapshot(&head_of(&c, "main").unwrap())
        .unwrap();
    assert_eq!(tip.parents, [first_tip]);
    assert_eq!(fs::read_to_string(c.root().join("b.txt")).unwrap(), "b\n");

    // Round trip: concurv work exported back to the same Git repository
    // continues Git's history (the imported commits keep their ids).
    write(c.root(), &[("c.txt", b"from concurv\n")]);
    save(&c, "concurv work").unwrap();
    publish(&c).unwrap();
    let bare = outer.path().join("mirror.git");
    git(
        outer.path(),
        &[
            "clone",
            "-q",
            "--bare",
            src.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    let gbare = GitRepo::open(&bare).unwrap();
    let o = export_with(&c, &gbare, &export_opts(false)).unwrap();
    assert_eq!(o.exported, 1, "only the concurv snapshot is new to Git");
    assert_eq!(git(&bare, &["rev-parse", "main^"]), c3);
    assert_eq!(git(&bare, &["rev-parse", "main~3"]), c1);

    // Git moves on from the exported commit; importing from the mirror
    // continues concurv's history again.
    let work = outer.path().join("work");
    git(
        outer.path(),
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            work.to_str().unwrap(),
        ],
    );
    commit(&work, &[("d.txt", b"d\n")], "git after concurv");
    git(&work, &["push", "-q", "origin", "main"]);
    let i = import_with(&c, &gbare, &import_opts("main", false)).unwrap();
    assert_eq!(i.commits, 1);
    assert!(i.published);
    assert!(c.root().join("d.txt").is_file());

    // A dirty workspace is never touched: the branch is published, the
    // files stay, and the outcome says the checkout did not happen.
    commit(&work, &[("e.txt", b"e\n")], "more git");
    git(&work, &["push", "-q", "origin", "main"]);
    write(c.root(), &[("a.txt", b"unsaved edit\n")]);
    let i = import_with(&c, &gbare, &import_opts("main", false)).unwrap();
    assert!(i.published && !i.checked_out);
    assert!(!c.root().join("e.txt").exists());
    assert_eq!(
        fs::read_to_string(c.root().join("a.txt")).unwrap(),
        "unsaved edit\n"
    );
    assert!(verify(&c, true).ok());
}

#[test]
fn diverged_histories_are_refused_or_imported_beside() {
    let outer = tempfile::tempdir().unwrap();
    let src = outer.path().join("src");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    commit(&src, &[("a.txt", b"one\n"), ("z.txt", b"z\n")], "first");
    let gsrc = GitRepo::open(&src).unwrap();
    let c = new_concurv(&outer.path().join("c"));
    import_with(&c, &gsrc, &import_opts("main", false)).unwrap();

    // Both sides move on.
    write(c.root(), &[("a.txt", b"concurv\n")]);
    save(&c, "concurv side").unwrap();
    publish(&c).unwrap();
    let concurv_main = head_of(&c, "main").unwrap();
    commit(&src, &[("z.txt", b"git\n")], "git side");

    // Into main: refused, nothing changed.
    let e = import_with(&c, &gsrc, &import_opts("main", false)).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert!(e.message.contains("--into"), "{}", e.message);
    assert_eq!(head_of(&c, "main"), Some(concurv_main));

    // Into a new branch, then merged like any branch.
    let i = import_with(&c, &gsrc, &import_opts("git-main", false)).unwrap();
    assert_eq!(i.commits, 1);
    assert!(i.published && !i.checked_out);
    let side = c
        .store()
        .read_snapshot(&head_of(&c, "git-main").unwrap())
        .unwrap();
    assert_eq!(side.message, "git side");
    merge_branch(&c, "git-main", UpdateOptions::default()).unwrap();
    publish(&c).unwrap();
    assert_eq!(
        fs::read_to_string(c.root().join("a.txt")).unwrap(),
        "concurv\n"
    );
    assert_eq!(fs::read_to_string(c.root().join("z.txt")).unwrap(), "git\n");

    // Later Git work fast-forwards the side branch; this workspace tracks
    // main and is left alone.
    commit(&src, &[("n.txt", b"n\n")], "newer");
    let i = import_with(&c, &gsrc, &import_opts("git-main", false)).unwrap();
    assert!(i.published && !i.checked_out);
    assert_eq!(i.commits, 1);
    assert!(!c.root().join("n.txt").exists());

    // A conflicting concurv tag refuses an import with --tags.
    git(&src, &["tag", "v1"]);
    tags::create(&c, "v1", Some(concurv_main)).unwrap();
    let before = head_of(&c, "git-main");
    commit(&src, &[("m.txt", b"m\n")], "more");
    git(&src, &["tag", "-f", "v1"]);
    let e = import_with(&c, &gsrc, &import_opts("git-main", true)).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert!(e.message.contains("v1"), "{}", e.message);
    assert_eq!(head_of(&c, "git-main"), before);
}
