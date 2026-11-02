#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! Differential test (ADR 0007 §5): concurv's Git rule layer for branch names
//! must agree with `git check-ref-format refs/heads/<name>` on every case,
//! and every name concurv accepts for a new branch must be accepted by Git.
//! Requires `git` on PATH (present on every CI runner).

use std::process::Command;

use concurv_workspace::refname::{git_refname_ok, portable};

fn git_accepts(name: &str) -> bool {
    Command::new("git")
        .args(["check-ref-format", &format!("refs/heads/{name}")])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .expect("git must be installed for this test")
        .status
        .success()
}

fn corpus() -> Vec<String> {
    let mut v: Vec<String> = [
        "main",
        "feature/login",
        "a",
        "a/b/c",
        "v1.0",
        "1",
        "-x",
        "x-",
        "HEAD",
        "@",
        "@@",
        "a@b",
        "a@{b",
        "@{",
        "a{b",
        "a}b",
        "a..b",
        "a.b",
        ".a",
        "a.",
        "a/.b",
        "a/b.",
        "a.lock",
        "a.lock/b",
        "a/b.lock",
        "lock",
        "a.locks",
        ".lock",
        "/a",
        "a/",
        "a//b",
        "a b",
        "a\tb",
        "a~b",
        "a^b",
        "a:b",
        "a?b",
        "a*b",
        "a[b",
        "a]b",
        "a\\b",
        "a!b",
        "a\"b",
        "a#b",
        "a$b",
        "a%b",
        "a&b",
        "a'b",
        "a(b",
        "a)b",
        "a+b",
        "a,b",
        "a;b",
        "a<b",
        "a=b",
        "a>b",
        "a|b",
        "a`b",
        "ü",
        "日本",
        "a\u{7f}b",
        "a\u{1}b",
        "...",
        "a...b",
        "a/./b",
        "a/../b",
        "refs/x",
        "remote/x",
        "con",
        "nul.txt",
        "x/aux",
        "a-b_c.d/e",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    // Every printable ASCII character at the start, middle and end.
    for c in (0x20u8..0x7f).map(char::from) {
        v.push(format!("{c}x"));
        v.push(format!("x{c}x"));
        v.push(format!("x{c}"));
        v.push(format!("x/{c}y/z"));
    }
    v
}

#[test]
fn git_rule_layer_matches_git() {
    let mut mismatches = Vec::new();
    for name in corpus() {
        let ours = git_refname_ok(&name);
        let theirs = git_accepts(&name);
        if ours != theirs {
            mismatches.push(format!("{name:?}: concurv {ours}, git {theirs}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "differences:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn every_portable_name_is_a_valid_git_branch() {
    for name in corpus() {
        if portable(&name).is_ok() {
            assert!(
                git_accepts(&name),
                "{name:?} accepted by concurv, refused by git"
            );
        }
    }
}
