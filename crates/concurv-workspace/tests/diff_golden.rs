#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Golden tests for `diff` output (R1.5, P0-8): missing final newline, CRLF
//! line endings, binary and added/deleted files.

mod common;

use std::fs;

use common::new_repo;
use concurv_workspace::{diff, save};

fn one_diff(old: &[u8], new: &[u8]) -> (Option<String>, Option<&'static str>) {
    let (d, repo) = new_repo();
    fs::write(d.path().join("f.txt"), old).unwrap();
    save(&repo, "base").unwrap();
    fs::write(d.path().join("f.txt"), new).unwrap();
    let mut diffs = diff(&repo).unwrap();
    assert_eq!(diffs.len(), 1);
    let fd = diffs.remove(0);
    (fd.unified, fd.note)
}

#[test]
fn added_missing_final_newline_is_marked() {
    let (u, _) = one_diff(b"a\nb\n", b"a\nb");
    assert_eq!(
        u.unwrap(),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\n-b\n+b\n\\ No newline at end of file\n"
    );
}

#[test]
fn removed_missing_final_newline_is_marked() {
    let (u, _) = one_diff(b"a\nb", b"a\nb\n");
    assert_eq!(
        u.unwrap(),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n"
    );
}

#[test]
fn both_sides_without_final_newline() {
    let (u, _) = one_diff(b"x", b"y");
    assert_eq!(
        u.unwrap(),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1 +1 @@\n-x\n\\ No newline at end of file\n+y\n\\ No newline at end of file\n"
    );
}

#[test]
fn crlf_line_endings_are_preserved() {
    let (u, _) = one_diff(b"a\r\nb\r\n", b"a\r\nc\r\n");
    assert_eq!(
        u.unwrap(),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n a\r\n-b\r\n+c\r\n"
    );
}

#[test]
fn line_ending_only_change_is_visible() {
    let (u, _) = one_diff(b"a\nb\n", b"a\r\nb\n");
    assert_eq!(
        u.unwrap(),
        "--- a/f.txt\n+++ b/f.txt\n@@ -1,2 +1,2 @@\n-a\n+a\r\n b\n"
    );
}

#[test]
fn binary_change_is_summarised() {
    let (u, note) = one_diff(b"\x00\x01\x02", b"\x00\x01\x03");
    assert!(u.is_none());
    assert_eq!(note, Some("binary files differ"));
}

#[test]
fn invalid_utf8_is_treated_as_binary() {
    let (u, note) = one_diff(b"ok\n", b"\xff\xfe\n");
    assert!(u.is_none());
    assert_eq!(note, Some("binary files differ"));
}

#[test]
fn added_and_deleted_files() {
    let (d, repo) = new_repo();
    fs::write(d.path().join("gone.txt"), b"old\n").unwrap();
    save(&repo, "base").unwrap();
    fs::remove_file(d.path().join("gone.txt")).unwrap();
    fs::write(d.path().join("new.txt"), b"fresh").unwrap();
    let diffs = diff(&repo).unwrap();
    let by: Vec<_> = diffs
        .iter()
        .map(|f| (f.change.path.to_string(), f.unified.clone().unwrap()))
        .collect();
    assert_eq!(
        by,
        vec![
            (
                "gone.txt".to_owned(),
                "--- a/gone.txt\n+++ b/gone.txt\n@@ -1 +0,0 @@\n-old\n".to_owned()
            ),
            (
                "new.txt".to_owned(),
                "--- a/new.txt\n+++ b/new.txt\n@@ -0,0 +1 @@\n+fresh\n\\ No newline at end of file\n"
                    .to_owned()
            ),
        ]
    );
}
