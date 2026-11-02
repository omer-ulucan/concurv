#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R1.5 P0-1…P0-5: Git-compatible ignore rules in the native scanner.

mod common;

use std::fs;

use common::*;
use concurv_core::{ErrorCode, RepoPath};
use concurv_workspace::ignore::{RuleSource, Verdict, check_ignore};
use concurv_workspace::{
    Repo, RestoreOptions, publish, restore, save, status, undo, update, workspaces,
};

/// Paths a save records, as strings.
fn saved_paths(repo: &Repo) -> Vec<String> {
    let s = save(repo, "s").unwrap();
    let head = s.snapshot.or(repo.head_snapshot().unwrap()).unwrap();
    repo.snapshot_files(&head)
        .unwrap()
        .keys()
        .map(|p| p.to_string())
        .collect()
}

fn changed(repo: &Repo) -> Vec<String> {
    status(repo)
        .unwrap()
        .changes
        .iter()
        .map(|c| format!("{} {}", c.kind.as_str(), c.path))
        .collect()
}

#[test]
fn build_outputs_dependencies_and_secrets_are_not_saved() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[
            (".gitignore", b"target/\nnode_modules/\n.env\n*.log\n"),
            ("src/main.rs", b"fn main() {}\n"),
            ("target/debug/app", b"binary"),
            ("node_modules/pkg/index.js", b"js"),
            (".env", b"SECRET=1"),
            ("sub/.env", b"SECRET=2"),
            ("debug.log", b"log"),
            ("sub/deep/trace.log", b"log"),
        ]),
    );
    assert_eq!(saved_paths(&repo), [".gitignore", "src/main.rs"]);
    // Ignored files are left alone on disk and not reported by status.
    assert!(d.path().join("target/debug/app").exists());
    assert!(changed(&repo).is_empty());
}

#[test]
fn gitignore_pattern_semantics() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[
            (
                ".gitignore",
                b"# comment line\n\\#hash.txt\n/anchored.txt\nonlydir/\n**/cache/**\na/**/z.txt\ntrailing.txt   \nescaped\\ space\n*.tmp\n!keep.tmp\n",
            ),
            ("#hash.txt", b"x"),
            ("anchored.txt", b"x"),
            ("nested/anchored.txt", b"kept: anchor is root-only"),
            ("onlydir/f", b"x"),
            ("x/onlydir", b"kept: a file, not a directory"),
            ("p/cache/q/r", b"x"),
            ("a/b/c/z.txt", b"x"),
            ("a/z.txt", b"x"),
            ("trailing.txt", b"x"),
            ("escaped space", b"x"),
            ("drop.tmp", b"x"),
            ("keep.tmp", b"kept: re-included"),
            ("plain.txt", b"kept"),
        ]),
    );
    assert_eq!(
        saved_paths(&repo),
        [
            ".gitignore",
            "keep.tmp",
            "nested/anchored.txt",
            "plain.txt",
            "x/onlydir"
        ]
    );
}

#[test]
fn nested_gitignore_overrides_and_excluded_parents_cannot_be_reincluded() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[
            (".gitignore", b"*.log\nbuild/\n!build/keep.txt\n"),
            ("app.log", b"x"),
            ("sub/.gitignore", b"!important.log\n"),
            ("sub/important.log", b"kept: deeper file wins"),
            ("sub/other.log", b"x"),
            ("build/keep.txt", b"x: parent excluded, like Git"),
            ("build/out.bin", b"x"),
        ]),
    );
    assert_eq!(
        saved_paths(&repo),
        [".gitignore", "sub/.gitignore", "sub/important.log"]
    );
}

#[test]
fn tracked_files_stay_tracked_after_an_ignore_rule_matches_them() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[("config.json", b"v1"), ("build/manifest.txt", b"m1")]),
    );
    save(&repo, "tracked").unwrap();
    // Later, rules that match the tracked files are added.
    fs::write(d.path().join(".gitignore"), "config.json\nbuild/\n").unwrap();
    fs::write(d.path().join("build/new-artifact.o"), b"untracked").unwrap();
    fs::write(d.path().join("config.json"), b"v2").unwrap();
    assert_eq!(
        changed(&repo),
        ["added .gitignore", "modified config.json"],
        "tracked files are still compared; untracked files in the ignored dir stay ignored"
    );
    let paths = saved_paths(&repo);
    assert_eq!(paths, [".gitignore", "build/manifest.txt", "config.json"]);
    // Deleting a tracked-but-ignored file is still a deletion.
    fs::remove_file(d.path().join("config.json")).unwrap();
    assert_eq!(changed(&repo), ["deleted config.json"]);
}

#[test]
fn local_exclude_applies_after_gitignore_files() {
    let (d, repo) = new_repo();
    fs::write(
        d.path().join(".concurv/exclude"),
        "*.local\nnotes/\nvip.local\n",
    )
    .unwrap();
    set_files(
        d.path(),
        &files(&[
            ("a.local", b"x"),
            ("notes/n.md", b"x"),
            (".gitignore", b"!vip.local\n"),
            ("vip.local", b"kept: .gitignore beats the local exclude"),
            ("ok.txt", b"kept"),
        ]),
    );
    assert_eq!(saved_paths(&repo), [".gitignore", "ok.txt", "vip.local"]);
    // The exclude file itself is never versioned (it lives in .concurv/).
}

#[test]
fn restore_never_overwrites_an_ignored_file_even_with_force() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[("secret.cfg", b"v1 tracked"), ("a", b"1")]),
    );
    let s1 = save(&repo, "with secret.cfg").unwrap().snapshot.unwrap();
    fs::remove_file(d.path().join("secret.cfg")).unwrap();
    fs::write(d.path().join(".gitignore"), "secret.cfg\n").unwrap();
    save(&repo, "untrack it").unwrap();
    // A new, ignored, untracked secret.cfg exists locally.
    fs::write(d.path().join("secret.cfg"), b"LOCAL ONLY - must survive").unwrap();
    for force in [false, true] {
        let e = restore(&repo, s1, RestoreOptions { force }).unwrap_err();
        assert_eq!(e.code, ErrorCode::DirtyWorkspace, "force={force}: {e}");
        assert!(e.message.contains("secret.cfg"), "{}", e.message);
        assert_eq!(
            fs::read(d.path().join("secret.cfg")).unwrap(),
            b"LOCAL ONLY - must survive"
        );
    }
    // Moving it away lets the restore proceed.
    fs::remove_file(d.path().join("secret.cfg")).unwrap();
    restore(&repo, s1, RestoreOptions::default()).unwrap();
    assert_eq!(
        fs::read(d.path().join("secret.cfg")).unwrap(),
        b"v1 tracked"
    );
}

#[test]
fn ignored_files_survive_restore_of_their_directory_and_undo() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[(".gitignore", b"*.cache\n"), ("dir/tracked.txt", b"t")]),
    );
    let s1 = save(&repo, "one").unwrap().snapshot.unwrap();
    fs::write(d.path().join("dir/x.cache"), b"ignored").unwrap();
    fs::remove_file(d.path().join("dir/tracked.txt")).unwrap();
    fs::write(d.path().join("other.txt"), b"o").unwrap();
    save(&repo, "two: dir emptied of tracked files").unwrap();
    // Restoring s1 brings dir/tracked.txt back; the ignored file stays.
    restore(&repo, s1, RestoreOptions::default()).unwrap();
    assert_eq!(fs::read(d.path().join("dir/x.cache")).unwrap(), b"ignored");
    // Undo the restore: tracked.txt goes again, the directory and the ignored
    // file remain (directories are only pruned when empty).
    undo(&repo).unwrap();
    assert!(!d.path().join("dir/tracked.txt").exists());
    assert_eq!(fs::read(d.path().join("dir/x.cache")).unwrap(), b"ignored");
}

#[test]
fn update_refuses_to_overwrite_an_ignored_file_in_another_workspace() {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    set_files(&ra, &files(&[("README", b"r")]));
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    workspaces::add(&a, &outer.path().join("b"), None).unwrap();
    let b = Repo::open(&outer.path().join("b"), opts()).unwrap();
    // B ignores (locally) a file that A then adds and publishes.
    fs::write(b.root().join(".concurv/exclude"), "tool.cfg\n").unwrap();
    fs::write(b.root().join("tool.cfg"), b"B's private config").unwrap();
    assert!(
        status(&b).unwrap().changes.is_empty(),
        "ignored: B is clean"
    );
    fs::write(ra.join("tool.cfg"), b"A's shared config").unwrap();
    save(&a, "add tool.cfg").unwrap();
    publish(&a).unwrap();
    let e = update(&b).unwrap_err();
    assert_eq!(e.code, ErrorCode::DirtyWorkspace, "{e}");
    assert_eq!(
        fs::read(b.root().join("tool.cfg")).unwrap(),
        b"B's private config"
    );
}

#[test]
fn resuming_a_restore_over_already_correct_ignored_content_is_allowed() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[("gen.out", b"generated v1"), ("a", b"1")]),
    );
    let s1 = save(&repo, "one").unwrap().snapshot.unwrap();
    fs::remove_file(d.path().join("gen.out")).unwrap();
    fs::write(d.path().join(".gitignore"), "*.out\n").unwrap();
    save(&repo, "two").unwrap();
    // Same bytes as the target already on disk: no data at risk.
    fs::write(d.path().join("gen.out"), b"generated v1").unwrap();
    restore(&repo, s1, RestoreOptions::default()).unwrap();
    assert_eq!(fs::read(d.path().join("gen.out")).unwrap(), b"generated v1");
}

/// Git compatibility found by probing the glob engine: Git has no brace
/// expansion (`{}` are literal), and Git never rejects a pattern (an
/// unusable one just never matches). concurv must do the same, and report
/// unusable lines through `check-ignore` instead of failing `status`/`save`.
#[test]
fn braces_are_literal_and_unusable_patterns_never_match() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[
            (
                ".gitignore",
                b"a{b,c}\nx{y\n[z-a]\ntrailing\\\nbad[pattern\n",
            ),
            ("a{b,c}", b"ignored: literal braces"),
            ("ab", b"kept: no brace expansion"),
            ("ac", b"kept"),
            ("x{y", b"ignored: literal"),
            // Known divergence (documented): for a reversed range Git matches
            // the first character, so Git would ignore `z`; concurv treats
            // `[z-a]` as unusable and never matches (it versions more, never less).
            ("z", b"kept by concurv; ignored by Git"),
            (
                "bad[pattern",
                b"kept: an unclosed [ never matches, as in Git",
            ),
        ]),
    );
    assert_eq!(
        saved_paths(&repo),
        [".gitignore", "ab", "ac", "bad[pattern", "z"]
    );
    let head = repo
        .head_files(repo.head_snapshot().unwrap().as_ref())
        .unwrap();
    let (_, unusable) = check_ignore(
        repo.root(),
        &head,
        &[(RepoPath::parse("z").unwrap(), false)],
    )
    .unwrap();
    let lines: Vec<usize> = unusable.iter().map(|u| u.line).collect();
    assert_eq!(
        lines,
        [3, 4, 5],
        "range, dangling backslash, unclosed class: {unusable:?}"
    );
}

#[test]
fn check_ignore_explains_decisions() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[
            (".gitignore", b"*.log\n!keep.log\nbuild/\n"),
            ("tracked.log", b"t"),
        ]),
    );
    // Track tracked.log while the rules are out of the way.
    fs::rename(d.path().join(".gitignore"), d.path().join("gi.tmp")).unwrap();
    save(&repo, "track tracked.log").unwrap();
    fs::rename(d.path().join("gi.tmp"), d.path().join(".gitignore")).unwrap();
    fs::create_dir(d.path().join("build")).unwrap();
    let head = repo
        .head_files(repo.head_snapshot().unwrap().as_ref())
        .unwrap();
    let p = |s: &str| (RepoPath::parse(s).unwrap(), false);
    let (r, _) = check_ignore(
        repo.root(),
        &head,
        &[
            p("debug.log"),
            p("keep.log"),
            p("build/out.o"),
            p("tracked.log"),
            p("src/x.rs"),
        ],
    )
    .unwrap();
    let root_rules = RuleSource::GitIgnore(RepoPath::root());
    assert!(r[0].ignored);
    assert_eq!(
        r[0].verdict,
        Verdict::Ignored {
            source: root_rules.clone(),
            pattern: "*.log".into()
        }
    );
    assert!(!r[1].ignored);
    assert!(
        matches!(&r[1].verdict, Verdict::Included { pattern, .. } if pattern.contains("keep.log"))
    );
    assert!(r[2].ignored);
    assert_eq!(
        r[2].excluded_parent,
        Some(RepoPath::parse("build").unwrap())
    );
    assert!(
        r[3].tracked && !r[3].ignored,
        "tracked files are never ignored"
    );
    assert!(!r[4].ignored && r[4].verdict == Verdict::Unmatched);
}

#[cfg(unix)]
#[test]
fn ignored_special_files_and_symlinked_gitignore() {
    use std::os::unix::fs::symlink;
    let (d, repo) = new_repo();
    fs::write(d.path().join(".gitignore"), "*.sock\n").unwrap();
    let st = std::process::Command::new("mkfifo")
        .arg(d.path().join("server.sock"))
        .status()
        .unwrap();
    assert!(st.success());
    fs::write(d.path().join("ok"), b"x").unwrap();
    // An ignored FIFO does not fail the scan (unignored ones still do).
    assert_eq!(saved_paths(&repo), [".gitignore", "ok"]);
    // A symlinked .gitignore is not followed as a rules file.
    fs::create_dir(d.path().join("sub")).unwrap();
    fs::write(d.path().join("rules.txt"), "*\n").unwrap();
    symlink("../rules.txt", d.path().join("sub/.gitignore")).unwrap();
    fs::write(d.path().join("sub/file"), b"kept").unwrap();
    let paths = saved_paths(&repo);
    assert!(paths.contains(&"sub/file".to_owned()), "{paths:?}");
}
