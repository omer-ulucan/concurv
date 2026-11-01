#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! End-to-end engine tests on real disk state (R1 gate items 1, 4, 8, 9).

mod common;

use std::fs;

use common::*;
use concurv_core::ErrorCode;
use concurv_workspace::{ChangeKind, Repo, log, save, status, verify};

#[test]
fn init_is_complete_and_refuses_reinit() {
    let (d, repo) = new_repo();
    for p in [
        "format",
        "device_id",
        "workspace_id",
        "op_head",
        "objects",
        "tmp",
    ] {
        assert!(d.path().join(".concurv").join(p).exists(), "{p}");
    }
    assert!(repo.head_snapshot().unwrap().is_none());
    let e = Repo::init(d.path(), opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::AlreadyInitialized);
    // No staging directories left behind.
    let leftovers = fs::read_dir(d.path())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name() != ".concurv")
        .count();
    assert_eq!(leftovers, 0);
    assert!(verify(&repo, true).ok());
}

#[test]
fn discover_from_subdirectory_and_not_a_repo() {
    let (d, _repo) = new_repo();
    fs::create_dir_all(d.path().join("a/b")).unwrap();
    let r = Repo::discover(&d.path().join("a/b"), opts()).unwrap();
    assert_eq!(
        fs::canonicalize(r.root()).unwrap(),
        fs::canonicalize(d.path()).unwrap()
    );
    let other = tempfile::tempdir().unwrap();
    assert_eq!(
        Repo::open(other.path(), opts()).unwrap_err().code,
        ErrorCode::NotARepo
    );
}

#[test]
fn unsupported_repo_format_is_refused() {
    let (d, _repo) = new_repo();
    fs::write(d.path().join(".concurv/format"), "concurv-repo-format 99\n").unwrap();
    assert_eq!(
        Repo::open(d.path(), opts()).unwrap_err().code,
        ErrorCode::UnsupportedFormat
    );
}

#[test]
fn edit_status_save_log_cycle() {
    let (d, repo) = new_repo();
    let root = d.path();
    set_files(
        root,
        &files(&[
            ("a.txt", b"one\n"),
            ("dir/b.bin", &[0, 1, 2, 255]),
            ("empty", b""),
        ]),
    );
    let st = status(&repo).unwrap();
    assert_eq!(st.changes.len(), 3);
    assert!(st.changes.iter().all(|c| c.kind == ChangeKind::Added));
    let s1 = save(&repo, "first").unwrap().snapshot.unwrap();
    assert!(status(&repo).unwrap().changes.is_empty());

    // Modify, delete, add; rename is delete + add.
    fs::write(root.join("a.txt"), b"two\n").unwrap();
    fs::remove_file(root.join("empty")).unwrap();
    fs::rename(root.join("dir/b.bin"), root.join("dir/c.bin")).unwrap();
    let st = status(&repo).unwrap();
    let got: Vec<_> = st
        .changes
        .iter()
        .map(|c| (c.path.to_string(), c.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("a.txt".to_string(), ChangeKind::Modified),
            ("dir/b.bin".to_string(), ChangeKind::Deleted),
            ("dir/c.bin".to_string(), ChangeKind::Added),
            ("empty".to_string(), ChangeKind::Deleted),
        ]
    );
    let s2 = save(&repo, "second\n\nbody").unwrap().snapshot.unwrap();
    let history = log(&repo, 10).unwrap();
    assert_eq!(history.iter().map(|h| h.0).collect::<Vec<_>>(), [s2, s1]);
    assert_eq!(history[0].1.parents, [s1]);
    assert_eq!(history[0].1.message, "second\n\nbody");

    // Nothing changed → no new snapshot or operation.
    let before = op_head(&repo);
    let o = save(&repo, "noop").unwrap();
    assert!(o.snapshot.is_none() && o.operation.is_none());
    assert_eq!(op_head(&repo), before);

    // Operation log is independent of snapshot history.
    let ops = repo.oplog().walk(100).unwrap();
    let kinds: Vec<_> = ops.iter().map(|(_, o)| o.kind.as_str()).collect();
    assert_eq!(kinds, ["save", "save", "init"]);
    assert!(verify(&repo, true).ok());
}

#[test]
fn empty_message_is_rejected() {
    let (_d, repo) = new_repo();
    assert_eq!(
        save(&repo, "  ").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn status_and_verify_do_not_mutate() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("x", b"1")]));
    save(&repo, "x").unwrap();
    fs::write(d.path().join("x"), b"2").unwrap();
    fs::write(d.path().join("new"), b"3").unwrap();
    let head = op_head(&repo);
    let objects = repo.store().list().unwrap().ids.len();
    for _ in 0..3 {
        status(&repo).unwrap();
        concurv_workspace::diff(&repo).unwrap();
        verify(&repo, true);
    }
    assert_eq!(op_head(&repo), head);
    assert_eq!(repo.store().list().unwrap().ids.len(), objects);
    assert_eq!(fs::read(d.path().join("x")).unwrap(), b"2");
}

/// Regression target from jj #10509: same size, same mtime, different bytes.
#[test]
fn status_detects_same_size_same_mtime_change() {
    let (d, repo) = new_repo();
    let p = d.path().join("f.txt");
    fs::write(&p, b"aaaa").unwrap();
    save(&repo, "a").unwrap();
    let mtime = fs::metadata(&p).unwrap().modified().unwrap();
    fs::write(&p, b"bbbb").unwrap();
    fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(fs::metadata(&p).unwrap().modified().unwrap(), mtime);
    let st = status(&repo).unwrap();
    assert_eq!(st.changes.len(), 1);
    assert_eq!(st.changes[0].kind, ChangeKind::Modified);
}

#[test]
fn diff_renders_text_and_binary() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[("t.txt", b"a\nb\nc\n"), ("b.bin", &[0, 1, 2])]),
    );
    save(&repo, "base").unwrap();
    fs::write(d.path().join("t.txt"), b"a\nB\nc\n").unwrap();
    fs::write(d.path().join("b.bin"), [0, 9, 2]).unwrap();
    let diffs = concurv_workspace::diff(&repo).unwrap();
    assert_eq!(diffs.len(), 2);
    let bin = &diffs[0];
    assert_eq!(bin.change.path.to_string(), "b.bin");
    assert!(bin.unified.is_none());
    assert_eq!(bin.note, Some("binary files differ"));
    let text = diffs[1].unified.as_ref().unwrap();
    assert!(text.contains("--- a/t.txt"), "{text}");
    assert!(text.contains("-b\n+B\n"), "{text}");
}

#[test]
fn metadata_dir_is_never_versioned() {
    let (d, repo) = new_repo();
    set_files(
        d.path(),
        &files(&[
            ("ok.txt", b"x"),
            ("sub/.concurv/evil", b"y"),
            ("sub/.CONCURV/evil2", b"z"),
        ]),
    );
    fs::create_dir(d.path().join(".concurv-init-0123")).unwrap();
    fs::write(d.path().join(".concurv-init-0123/f"), b"debris").unwrap();
    let s = save(&repo, "s").unwrap().snapshot.unwrap();
    let files = repo.snapshot_files(&s).unwrap();
    let paths: Vec<_> = files.keys().map(|p| p.to_string()).collect();
    assert_eq!(paths, ["ok.txt"]);
}

/// Same content in independent repositories and runs → identical tree ids,
/// pinned to a golden value so the hash is stable across runs and OSes.
#[test]
fn deterministic_tree_ids_across_repositories() {
    let content = files(&[
        ("README.md", b"# fixture\n"),
        ("src/main.rs", b"fn main() {}\n"),
        ("src/zz/deep.txt", b"deep"),
        ("bin.dat", &[0, 0, 255, 7]),
        ("empty", b""),
        ("\u{e9}t\u{e9}.txt", "unicode name".as_bytes()),
    ]);
    let mut trees = Vec::new();
    for _ in 0..2 {
        let (d, repo) = new_repo();
        set_files(d.path(), &content);
        let s = save(&repo, "m").unwrap().snapshot.unwrap();
        trees.push(repo.store().read_snapshot(&s).unwrap().tree);
    }
    assert_eq!(trees[0], trees[1]);
    assert_eq!(trees[0].to_hex(), GOLDEN_FIXTURE_TREE);
}

const GOLDEN_FIXTURE_TREE: &str =
    "047aec882a9e5b6d02f539d60d303929c91666393a5ae47815c3acd9147b69c9";

#[test]
fn empty_directories_are_not_versioned() {
    let (d, repo) = new_repo();
    fs::create_dir_all(d.path().join("a/b/c")).unwrap();
    fs::write(d.path().join("f"), b"1").unwrap();
    let s = save(&repo, "s").unwrap().snapshot.unwrap();
    assert_eq!(repo.snapshot_files(&s).unwrap().len(), 1);
    assert!(verify(&repo, true).ok());
}

#[test]
fn concurrent_writer_is_locked_out() {
    let (d, repo) = new_repo();
    fs::write(d.path().join("f"), b"1").unwrap();
    let _held = repo.lock().unwrap();
    let other = Repo::open(d.path(), opts()).unwrap();
    assert_eq!(save(&other, "x").unwrap_err().code, ErrorCode::Locked);
    assert!(other.head_snapshot().unwrap().is_none());
}

#[test]
fn corrupt_reachable_object_fails_verify_and_save_self_heals() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("a", b"alpha"), ("b", b"beta")]));
    let s = save(&repo, "s").unwrap().snapshot.unwrap();
    let blob = repo
        .snapshot_files(&s)
        .unwrap()
        .values()
        .next()
        .unwrap()
        .blob;
    let path = repo.store().path_for(&blob.object_id());
    fs::write(&path, b"CNV1junk").unwrap();
    let rep = verify(&repo, true);
    assert!(!rep.ok());
    assert!(
        rep.problems
            .iter()
            .any(|p| p.code == ErrorCode::ObjectCorrupt)
    );

    // Deleting a reachable object is reported as missing.
    fs::remove_file(&path).unwrap();
    let rep = verify(&repo, false);
    assert!(
        rep.problems
            .iter()
            .any(|p| p.code == ErrorCode::ObjectMissing)
    );

    // The working copy still has the content; saving re-creates the object
    // (dedup sees it missing and writes it again), healing the history.
    fs::write(d.path().join("c"), b"new").unwrap();
    save(&repo, "heal").unwrap();
    assert!(
        verify(&repo, true).ok(),
        "{:?}",
        verify(&repo, true).problems
    );
}

#[test]
fn missing_snapshot_member_is_detected() {
    let (d, repo) = new_repo();
    set_files(d.path(), &files(&[("dir/x", b"1")]));
    let s = save(&repo, "s").unwrap().snapshot.unwrap();
    let tree = repo.store().read_snapshot(&s).unwrap().tree;
    fs::remove_file(repo.store().path_for(&tree.object_id())).unwrap();
    let rep = verify(&repo, false);
    assert!(
        rep.problems
            .iter()
            .any(|p| p.code == ErrorCode::ObjectMissing)
    );
    assert_eq!(status(&repo).unwrap_err().code, ErrorCode::ObjectMissing);
}

#[cfg(unix)]
#[test]
fn unix_exec_bits_and_symlinks_roundtrip() {
    use concurv_objects::EntryKind;
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (d, repo) = new_repo();
    let root = d.path();
    fs::write(root.join("run.sh"), b"#!/bin/sh\n").unwrap();
    fs::set_permissions(root.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(root.join("plain"), b"p").unwrap();
    symlink("plain", root.join("link")).unwrap();
    symlink("/definitely/outside", root.join("abs")).unwrap();
    let s1 = save(&repo, "modes").unwrap().snapshot.unwrap();
    let f = repo.snapshot_files(&s1).unwrap();
    let kind = |p: &str| f[&concurv_core::RepoPath::parse(p).unwrap()].kind;
    assert_eq!(kind("run.sh"), EntryKind::Executable);
    assert_eq!(kind("plain"), EntryKind::File);
    assert_eq!(kind("link"), EntryKind::Symlink);

    // chmod alone is a type change.
    fs::set_permissions(root.join("plain"), fs::Permissions::from_mode(0o755)).unwrap();
    let st = status(&repo).unwrap();
    assert_eq!(st.changes[0].kind, ChangeKind::TypeChanged);

    // Restore recreates links without following them.
    common::clear(root);
    concurv_workspace::restore(&repo, s1, concurv_workspace::RestoreOptions { force: true })
        .unwrap();
    assert_eq!(
        fs::read_link(root.join("link")).unwrap().to_str(),
        Some("plain")
    );
    assert_eq!(
        fs::read_link(root.join("abs")).unwrap().to_str(),
        Some("/definitely/outside")
    );
    let mode = fs::metadata(root.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0);
}

#[cfg(unix)]
#[test]
fn unix_fifo_is_rejected() {
    let (d, repo) = new_repo();
    let st = std::process::Command::new("mkfifo")
        .arg(d.path().join("pipe"))
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(
        save(&repo, "s").unwrap_err().code,
        ErrorCode::UnsupportedFileType
    );
    assert!(repo.head_snapshot().unwrap().is_none());
}

/// Non-UTF-8 names are rejected by `save`. macOS (APFS) refuses to create
/// such names at all (`EILSEQ`), so there the test asserts that refusal: the
/// name can never reach concurv on that platform.
#[cfg(unix)]
#[test]
fn unix_non_utf8_names_are_rejected() {
    use std::os::unix::ffi::OsStrExt;
    let (d, repo) = new_repo();
    let macos = std::env::consts::OS == "macos";
    let bad = std::ffi::OsStr::from_bytes(b"bad\xffname");
    match fs::write(d.path().join(bad), b"x") {
        Ok(()) => {
            assert!(!macos, "macOS unexpectedly allowed a non-UTF-8 name");
            assert_eq!(save(&repo, "s").unwrap_err().code, ErrorCode::InvalidPath);
            assert!(repo.head_snapshot().unwrap().is_none());
        }
        Err(e) => {
            const EILSEQ_MACOS: i32 = 92;
            assert!(
                macos && e.raw_os_error() == Some(EILSEQ_MACOS),
                "unexpected error creating a non-UTF-8 name: {e}"
            );
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_junctions_are_rejected_not_followed() {
    let (d, repo) = new_repo();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), b"outside data").unwrap();
    let st = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(d.path().join("j"))
        .arg(outside.path())
        .output()
        .unwrap();
    assert!(st.status.success(), "{st:?}");
    let e = save(&repo, "s").unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedFileType, "{e}");
    assert!(repo.head_snapshot().unwrap().is_none());
    fs::remove_dir(d.path().join("j")).unwrap();
    assert_eq!(
        fs::read(outside.path().join("secret")).unwrap(),
        b"outside data"
    );
}

#[cfg(windows)]
#[test]
fn windows_unpaired_surrogate_name_is_rejected() {
    use std::os::windows::ffi::OsStringExt;
    let (d, repo) = new_repo();
    let name = std::ffi::OsString::from_wide(&[0x61, 0xD800, 0x62]);
    fs::write(d.path().join(&name), b"x").unwrap();
    assert_eq!(save(&repo, "s").unwrap_err().code, ErrorCode::InvalidPath);
    assert!(repo.head_snapshot().unwrap().is_none());
}
