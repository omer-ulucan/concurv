#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2.5 P1-1: line-level merge in `update` (ADR 0006).

mod common;

use std::fs;

use common::*;
use concurv_core::ErrorCode;
use concurv_workspace::{
    Repo, UpdateKind, UpdateOptions, publish, save, update, update_with, verify, workspaces,
};

/// A and B start from `base`; A publishes `a_files`; B saves `b_files`.
fn diverged(base: &Files, a_files: &Files, b_files: &Files) -> (tempfile::TempDir, Repo, Repo) {
    let outer = tempfile::tempdir().unwrap();
    let ra = outer.path().join("a");
    fs::create_dir(&ra).unwrap();
    let a = Repo::init(&ra, opts()).unwrap();
    set_files(&ra, base);
    save(&a, "base").unwrap();
    publish(&a).unwrap();
    workspaces::add(&a, &outer.path().join("b"), None).unwrap();
    let b = Repo::open(&outer.path().join("b"), opts()).unwrap();
    set_files(&ra, a_files);
    save(&a, "a").unwrap();
    publish(&a).unwrap();
    set_files(b.root(), b_files);
    save(&b, "b").unwrap();
    (outer, a, b)
}

const BASE: &[u8] = b"fn one() {}\n\nfn two() {}\n\nfn three() {}\n";

#[test]
fn disjoint_edits_to_one_file_merge_and_publish() {
    let (_o, a, b) = diverged(
        &files(&[("lib.rs", BASE), ("x", b"x")]),
        &files(&[
            (
                "lib.rs",
                b"fn one() { 1 }\n\nfn two() {}\n\nfn three() {}\n",
            ),
            ("x", b"x"),
        ]),
        &files(&[
            (
                "lib.rs",
                b"fn one() {}\n\nfn two() {}\n\nfn three() { 3 }\n",
            ),
            ("x", b"x"),
        ]),
    );
    let out = update(&b).unwrap();
    assert_eq!(out.kind, UpdateKind::Merged);
    assert_eq!(
        out.content_merged
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["lib.rs"]
    );
    let merged = b"fn one() { 1 }\n\nfn two() {}\n\nfn three() { 3 }\n";
    assert_eq!(fs::read(b.root().join("lib.rs")).unwrap(), merged);
    // The merge snapshot is a fast-forward of main and verifies.
    publish(&b).unwrap();
    update(&a).unwrap();
    assert_eq!(fs::read(a.root().join("lib.rs")).unwrap(), merged);
    assert!(verify(&a, true).ok());
}

#[test]
fn overlapping_edits_still_conflict_and_change_nothing() {
    let (_o, _a, b) = diverged(
        &files(&[("lib.rs", BASE)]),
        &files(&[(
            "lib.rs",
            b"fn one() { 1 }\n\nfn two() {}\n\nfn three() {}\n",
        )]),
        &files(&[(
            "lib.rs",
            b"fn one() { 2 }\n\nfn two() {}\n\nfn three() {}\n",
        )]),
    );
    let before = read_disk(b.root());
    let head = b.head_snapshot().unwrap();
    let e = update(&b).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.message.contains("lib.rs"), "{e}");
    assert_eq!(read_disk(b.root()), before);
    assert_eq!(b.head_snapshot().unwrap(), head);
}

#[test]
fn mixed_merge_records_only_real_conflicts() {
    let (_o, _a, b) = diverged(
        &files(&[("merge.txt", b"1\n2\n3\n4\n5\n"), ("clash.txt", b"c\n")]),
        &files(&[("merge.txt", b"ONE\n2\n3\n4\n5\n"), ("clash.txt", b"a\n")]),
        &files(&[("merge.txt", b"1\n2\n3\n4\nFIVE\n"), ("clash.txt", b"b\n")]),
    );
    let out = update_with(
        &b,
        UpdateOptions {
            allow_conflicts: true,
        },
    )
    .unwrap();
    assert_eq!(out.kind, UpdateKind::Conflicted);
    let (_, entries) = out.conflict.unwrap();
    let paths: Vec<String> = entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(paths, ["clash.txt"]);
    assert_eq!(
        fs::read(b.root().join("merge.txt")).unwrap(),
        b"ONE\n2\n3\n4\nFIVE\n"
    );
}

#[test]
fn binary_and_symlink_changes_are_never_merged_by_content() {
    let (_o, _a, b) = diverged(
        &files(&[("img.bin", b"\0a\nb\nc\nd\n")]),
        &files(&[("img.bin", b"\0A\nb\nc\nd\n")]),
        &files(&[("img.bin", b"\0a\nb\nc\nD\n")]),
    );
    assert_eq!(update(&b).unwrap_err().code, ErrorCode::Conflict);
}

#[test]
fn crlf_and_missing_final_newline_merge_byte_exactly() {
    let (_o, _a, b) = diverged(
        &files(&[("w.txt", b"a\r\nb\r\nc\r\nd\r\ne")]),
        &files(&[("w.txt", b"A\r\nb\r\nc\r\nd\r\ne")]),
        &files(&[("w.txt", b"a\r\nb\r\nc\r\nd\r\nE")]),
    );
    update(&b).unwrap();
    assert_eq!(
        fs::read(b.root().join("w.txt")).unwrap(),
        b"A\r\nb\r\nc\r\nd\r\nE"
    );
}
