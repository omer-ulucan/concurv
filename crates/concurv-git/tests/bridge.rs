#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2 slice 2d: Git export/import (ADR 0002). Requires `git` on PATH.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use concurv_core::{ErrorCode, SnapshotId};
use concurv_git::{GitRepo, export, import};
use concurv_workspace::{Repo, RepoOptions, publish, save, update, verify, workspaces};

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

fn bare(dir: &Path) -> GitRepo {
    fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "--bare", "-b", "main"]);
    GitRepo::open(dir).unwrap()
}

fn write(root: &Path, files: &[(&str, &[u8])]) {
    for (p, c) in files {
        let f = root.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, c).unwrap();
    }
}

/// A concurv history with a merge, binary content, nested and Unicode paths.
fn concurv_history(outer: &Path) -> Repo {
    let ra = outer.join("a");
    fs::create_dir_all(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    write(
        &ra,
        &[("README.md", b"# demo\n"), ("src/lib.rs", b"fn a() {}\n")],
    );
    save(&a, "initial import").unwrap();
    publish(&a).unwrap();
    workspaces::add(&a, &outer.join("b"), None).unwrap();
    let b = Repo::open(&outer.join("b"), opts()).unwrap();
    write(
        &ra,
        &[
            ("src/lib.rs", b"fn a() { 1 }\n"),
            ("bin/data.bin", &[0, 159, 146, 150, 0]),
        ],
    );
    save(&a, "a: work\n\nwith a body").unwrap();
    publish(&a).unwrap();
    write(b.root(), &[("naïve dir/sp ace.txt", "üñí\n".as_bytes())]);
    save(&b, "b: unicode").unwrap();
    update(&b).unwrap(); // merge
    publish(&b).unwrap();
    update(&a).unwrap(); // fast-forward a to the merge
    a
}

fn history(repo: &Repo, tip: SnapshotId) -> Vec<(SnapshotId, concurv_objects::Snapshot)> {
    let mut out = Vec::new();
    let mut stack = vec![tip];
    let mut seen = std::collections::HashSet::new();
    while let Some(s) = stack.pop() {
        if !seen.insert(s) {
            continue;
        }
        let snap = repo.store().read_snapshot(&s).unwrap();
        stack.extend(snap.parents.iter().copied());
        out.push((s, snap));
    }
    out
}

fn main_head(repo: &Repo) -> SnapshotId {
    repo.current().unwrap().1.view.branches["main"]
}

#[test]
fn export_then_import_roundtrips_trees_history_and_metadata() {
    let outer = tempfile::tempdir().unwrap();
    let a = concurv_history(outer.path());
    let g = bare(&outer.path().join("mirror.git"));
    let o = export(&a, &g, "main").unwrap();
    assert!(o.updated && o.previous.is_none());
    assert_eq!(o.exported, 4);
    assert_eq!(git(g.path(), &["rev-list", "--count", "main"]), "4");
    assert_eq!(
        git(g.path(), &["rev-list", "--merges", "--count", "main"]),
        "1"
    );
    let files = git(g.path(), &["ls-tree", "-r", "--name-only", "main"]);
    assert!(
        files.contains("naïve dir/sp ace.txt")
            || files.contains("\"na\\303\\257ve dir/sp ace.txt\""),
        "{files}"
    );
    let msg = git(g.path(), &["log", "-1", "--format=%B", "main~1"]);
    assert!(msg.contains("Concurv-Change-Id: "), "{msg}");
    assert_eq!(
        git(g.path(), &["log", "-1", "--format=%an <%ae> %at", "main"]),
        "Ada Lovelace <ada@example.com> 1793491200"
    );

    // Import into a fresh repository: same trees, shape and metadata.
    let rc = outer.path().join("c");
    fs::create_dir(&rc).unwrap();
    let c = Repo::init(&rc, opts()).unwrap();
    let i = import(&c, &g, "main").unwrap();
    assert_eq!((i.commits, i.git_tip.as_str()), (4, o.tip.as_str()));
    let ha = history(&a, main_head(&a));
    let hc = history(&c, main_head(&c));
    assert_eq!(ha.len(), hc.len());
    let by_change: BTreeMap<_, _> = hc.iter().map(|(_, s)| (s.change_id, s.clone())).collect();
    for (_, sa) in &ha {
        let sc = &by_change[&sa.change_id];
        assert_eq!(
            sc.tree, sa.tree,
            "tree ids are identical across the round-trip"
        );
        assert_eq!(sc.parents.len(), sa.parents.len());
        assert_eq!(sc.message, sa.message.trim_end_matches('\n'));
        assert_eq!(sc.author, sa.author);
        assert_eq!(sc.timestamp_ms, sa.timestamp_ms / 1000 * 1000);
    }
    // The working copy was checked out, and everything verifies.
    assert_eq!(
        fs::read(rc.join("bin/data.bin")).unwrap(),
        [0, 159, 146, 150, 0]
    );
    assert_eq!(
        fs::read_to_string(rc.join("naïve dir/sp ace.txt")).unwrap(),
        "üñí\n"
    );
    assert!(verify(&c, true).ok());

    // Re-exporting the imported repository reproduces the same Git commits.
    let g2 = bare(&outer.path().join("mirror2.git"));
    assert_eq!(
        export(&c, &g2, "main").unwrap().tip,
        o.tip,
        "export is deterministic"
    );
}

#[test]
fn export_is_incremental_and_never_moves_a_diverged_git_branch() {
    let outer = tempfile::tempdir().unwrap();
    let a = concurv_history(outer.path());
    let g = bare(&outer.path().join("mirror.git"));
    let first = export(&a, &g, "main").unwrap();
    let again = export(&a, &g, "main").unwrap();
    assert_eq!((again.exported, again.updated), (0, false));
    assert_eq!(again.tip, first.tip);

    write(a.root(), &[("NEW", b"new\n")]);
    save(&a, "new file").unwrap();
    publish(&a).unwrap();
    let inc = export(&a, &g, "main").unwrap();
    assert_eq!((inc.exported, inc.reused), (1, 4));
    assert_eq!(git(g.path(), &["rev-parse", "main~1"]), first.tip);

    // Someone commits directly in Git: export must refuse, Git unchanged.
    let clone = outer.path().join("clone");
    git(
        outer.path(),
        &[
            "clone",
            "-q",
            g.path().to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    fs::write(clone.join("git-only.txt"), "x").unwrap();
    git(&clone, &["add", "git-only.txt"]);
    git(&clone, &["commit", "-q", "-m", "direct git commit"]);
    git(&clone, &["push", "-q", "origin", "main"]);
    let git_head = git(g.path(), &["rev-parse", "main"]);
    write(a.root(), &[("NEWER", b"newer\n")]);
    save(&a, "newer").unwrap();
    publish(&a).unwrap();
    let e = export(&a, &g, "main").unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert_eq!(git(g.path(), &["rev-parse", "main"]), git_head);
    // Exporting to a new branch works.
    assert!(export(&a, &g, "concurv-main").unwrap().updated);
}

#[test]
fn import_of_a_native_git_repository() {
    let outer = tempfile::tempdir().unwrap();
    let src = outer.path().join("src");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    write(
        &src,
        &[
            ("a.txt", b"one\n"),
            ("dir/deep/b.txt", b"b\n"),
            ("gone.txt", b"g\n"),
        ],
    );
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "first"]);
    git(&src, &["checkout", "-q", "-b", "side"]);
    write(&src, &[("side.txt", b"side\n")]);
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "side work"]);
    git(&src, &["checkout", "-q", "main"]);
    git(&src, &["rm", "-q", "gone.txt"]);
    write(&src, &[("a.txt", b"two\n")]);
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "second\n\nbody text"]);
    git(
        &src,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    );

    let rc = outer.path().join("c");
    fs::create_dir(&rc).unwrap();
    let c = Repo::init(&rc, opts()).unwrap();
    let i = import(&c, &GitRepo::open(&src).unwrap(), "main").unwrap();
    assert_eq!(i.commits, 4);
    let tip = c.store().read_snapshot(&main_head(&c)).unwrap();
    assert_eq!(tip.parents.len(), 2, "merge commit imported as a merge");
    assert_eq!(tip.message, "merge side");
    assert_eq!(tip.author, "Grace <grace@example.com>");
    assert_eq!(tip.timestamp_ms, 1_700_000_000_000);
    let mut on_disk: Vec<String> = Vec::new();
    for p in ["a.txt", "dir/deep/b.txt", "side.txt", "gone.txt"] {
        if rc.join(p).exists() {
            on_disk.push(p.to_owned());
        }
    }
    assert_eq!(on_disk, ["a.txt", "dir/deep/b.txt", "side.txt"]);
    assert_eq!(fs::read_to_string(rc.join("a.txt")).unwrap(), "two\n");
    let second = history(&c, main_head(&c))
        .into_iter()
        .find(|(_, s)| s.message.starts_with("second"))
        .unwrap();
    assert_eq!(second.1.message, "second\n\nbody text");
    assert!(verify(&c, true).ok());

    // Importing again into the same (now non-empty) repository is refused.
    let e = import(&c, &GitRepo::open(&src).unwrap(), "main").unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
}

#[test]
fn import_refuses_what_concurv_cannot_represent() {
    let outer = tempfile::tempdir().unwrap();
    let src = outer.path().join("src");
    fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    write(&src, &[("ok.txt", b"ok\n")]);
    git(&src, &["add", "-A"]);
    git(&src, &["commit", "-q", "-m", "ok"]);
    let head = git(&src, &["rev-parse", "HEAD"]);
    // A submodule entry and a `.concurv` path, added via the index directly.
    git(
        &src,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{head},vendor/lib"),
        ],
    );
    let blob = git(&src, &["hash-object", "-w", "ok.txt"]);
    git(
        &src,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},.concurv/evil"),
        ],
    );
    git(&src, &["commit", "-q", "-m", "unsupported"]);

    let rc = outer.path().join("c");
    fs::create_dir(&rc).unwrap();
    let c = Repo::init(&rc, opts()).unwrap();
    let e = import(&c, &GitRepo::open(&src).unwrap(), "main").unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedFileType);
    assert!(
        e.message.contains("vendor/lib") && e.message.contains("submodule"),
        "{}",
        e.message
    );
    assert!(e.message.contains(".concurv/evil"), "{}", e.message);
    // Nothing was published or checked out.
    assert!(c.current().unwrap().1.view.branches.is_empty());
    assert!(!rc.join("ok.txt").exists());
    assert_eq!(
        import(&c, &GitRepo::open(&src).unwrap(), "nope")
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let not_git = tempfile::tempdir().unwrap();
    assert_eq!(
        GitRepo::open(not_git.path()).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}
