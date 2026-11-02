#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Differential test: concurv's ignore decisions must equal real Git's.
//!
//! For each corpus case, the same files and ignore rules are written into a
//! directory that is both a concurv workspace and a Git repository. Git's
//! `ls-files --others --ignored --exclude-standard` is the reference for
//! which untracked files are ignored; concurv's view is "every file that
//! `status` does not report as added". Requires `git` on PATH (present on
//! every CI runner).

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;

use common::*;
use concurv_workspace::{Repo, status};

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.ignorecase=false", "-c", "core.excludesFile="])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
        .expect("git must be installed for this test");
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Files Git ignores (untracked), relative, `/`-separated.
fn git_ignored(dir: &Path) -> BTreeSet<String> {
    git(
        dir,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "-z",
        ],
    )
    .split('\0')
    .filter(|s| !s.is_empty())
    .filter(|s| !s.starts_with(".concurv/"))
    .map(str::to_owned)
    .collect()
}

/// Files concurv ignores: every corpus file `status` does not report.
fn concurv_ignored(repo: &Repo, corpus: &Files) -> BTreeSet<String> {
    let seen: BTreeSet<String> = status(repo)
        .unwrap()
        .changes
        .iter()
        .map(|c| c.path.to_string())
        .collect();
    corpus
        .keys()
        .filter(|p| !seen.contains(*p))
        .cloned()
        .collect()
}

fn check_case(name: &str, corpus: Files, local_exclude: Option<&str>) {
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), opts()).unwrap();
    set_files(d.path(), &corpus);
    git(d.path(), &["init", "-q"]);
    if let Some(ex) = local_exclude {
        fs::write(d.path().join(".concurv/exclude"), ex).unwrap();
        fs::write(d.path().join(".git/info/exclude"), ex).unwrap();
    }
    let expected = git_ignored(d.path());
    let actual = concurv_ignored(&repo, &corpus);
    assert_eq!(
        actual, expected,
        "case `{name}`: concurv (left) vs Git (right)"
    );
}

#[test]
fn common_project_layouts() {
    check_case(
        "rust+node project",
        files(&[
            (
                ".gitignore",
                b"/target\nnode_modules/\n.env\n*.log\n.DS_Store\n",
            ),
            ("Cargo.toml", b"x"),
            ("src/main.rs", b"x"),
            ("target/debug/app", b"x"),
            ("crates/a/target/x", b"x"),
            ("web/node_modules/pkg/i.js", b"x"),
            ("web/app.js", b"x"),
            (".env", b"x"),
            ("conf/.env", b"x"),
            ("logs/a.log", b"x"),
            (".DS_Store", b"x"),
        ]),
        None,
    );
}

#[test]
fn anchoring_directories_and_double_star() {
    check_case(
        "anchors",
        files(&[
            (
                ".gitignore",
                b"/root-only.txt\nonlydir/\n**/cache/**\na/**/z.txt\ndocs/*.md\nfoo/bar\n**/deep\n",
            ),
            ("root-only.txt", b"x"),
            ("sub/root-only.txt", b"x"),
            ("onlydir/f", b"x"),
            ("x/onlydir", b"x"),
            ("p/cache/q/r", b"x"),
            ("cache/top", b"x"),
            ("a/z.txt", b"x"),
            ("a/b/c/z.txt", b"x"),
            ("docs/readme.md", b"x"),
            ("docs/sub/nested.md", b"x"),
            ("foo/bar", b"x"),
            ("x/foo/bar", b"x"),
            ("one/two/deep", b"x"),
            ("deep", b"x"),
        ]),
        None,
    );
}

#[test]
fn negation_nesting_and_excluded_parents() {
    check_case(
        "negation",
        files(&[
            (
                ".gitignore",
                b"*.log\n!keep.log\nbuild/\n!build/keep.txt\nout/*\n!out/keep\n",
            ),
            ("a.log", b"x"),
            ("keep.log", b"x"),
            ("sub/keep.log", b"x"),
            ("sub/.gitignore", b"!important.log\n*.cfg\n"),
            ("sub/important.log", b"x"),
            ("sub/other.log", b"x"),
            ("sub/x.cfg", b"x"),
            ("x.cfg", b"x"),
            ("build/keep.txt", b"x"),
            ("build/out.bin", b"x"),
            ("out/keep", b"x"),
            ("out/drop", b"x"),
        ]),
        None,
    );
}

#[test]
fn comments_escapes_whitespace_and_classes() {
    check_case(
        "syntax",
        files(&[
            (
                ".gitignore",
                b"# a comment\n\\#hash.txt\n\\!bang.txt\ntrailing.txt   \n[abc]x.dat\n[!a]y.dat\n?q.dat\n*.[oa]\n",
            ),
            ("#hash.txt", b"x"),
            ("!bang.txt", b"x"),
            ("trailing.txt", b"x"),
            ("ax.dat", b"x"),
            ("dx.dat", b"x"),
            ("ay.dat", b"x"),
            ("by.dat", b"x"),
            ("zq.dat", b"x"),
            ("m.o", b"x"),
            ("m.a", b"x"),
            ("m.c", b"x"),
            ("plain", b"x"),
        ]),
        None,
    );
}

#[test]
fn braces_and_unusable_patterns() {
    check_case(
        "unusual patterns",
        files(&[
            (".gitignore", b"a{b,c}\nx{y\nbad[pattern\n"),
            ("a{b,c}", b"x"),
            ("ab", b"x"),
            ("ac", b"x"),
            ("x{y", b"x"),
            ("bad[pattern", b"x"),
        ]),
        None,
    );
}

#[test]
fn local_exclude_precedence() {
    check_case(
        "exclude",
        files(&[
            (".gitignore", b"!vip.local\n"),
            ("a.local", b"x"),
            ("vip.local", b"x"),
            ("notes/n.md", b"x"),
            ("ok", b"x"),
        ]),
        Some("*.local\nnotes/\n"),
    );
}

#[test]
fn nested_repositories_are_not_scanned() {
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), opts()).unwrap();
    set_files(
        d.path(),
        &files(&[("top.txt", b"x"), ("vendor/lib/src.c", b"x")]),
    );
    git(&d.path().join("vendor/lib"), &["init", "-q"]);
    let added: Vec<String> = status(&repo)
        .unwrap()
        .changes
        .iter()
        .map(|c| c.path.to_string())
        .collect();
    assert_eq!(
        added,
        ["top.txt"],
        "nested Git repository and .git internals skipped"
    );
    // A Git repository at the workspace root: `.git` itself is never scanned.
    git(d.path(), &["init", "-q"]);
    let added: Vec<String> = status(&repo)
        .unwrap()
        .changes
        .iter()
        .map(|c| c.path.to_string())
        .collect();
    assert_eq!(added, ["top.txt"]);
}
