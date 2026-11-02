#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! False-clean suite for the stat cache (R1.5 P0-7, ADR 0005). Each test
//! makes the cache as populated as possible and then checks that `status`
//! still sees every change. Where the cache is not trusted (Windows) the same
//! assertions must hold through full hashing.

mod common;

use std::fs;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

use common::{files, opts, set_files};
use concurv_workspace::statcache::STAT_CACHE;
use concurv_workspace::{
    ChangeKind, Repo, RepoOptions, StatusOptions, diff, save, status, status_with,
};

const WINDOW: Duration = Duration::from_millis(100);

fn cache_opts() -> RepoOptions {
    RepoOptions {
        stat_cache_window: Some(WINDOW),
        ..opts()
    }
}

fn repo_with(entries: &[(&str, &[u8])]) -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), cache_opts()).unwrap();
    set_files(d.path(), &files(entries));
    // Let the files' timestamps leave the racy window, so `save` caches them.
    sleep(WINDOW * 3);
    save(&repo, "base").unwrap();
    (d, repo)
}

fn changes(repo: &Repo) -> Vec<(String, ChangeKind)> {
    status(repo)
        .unwrap()
        .changes
        .into_iter()
        .map(|c| (c.path.to_string(), c.kind))
        .collect()
}

fn cache_file(root: &Path) -> std::path::PathBuf {
    root.join(".concurv").join(STAT_CACHE)
}

/// Rewrite `p` with `data` (same length) and put the old mtime back.
fn rewrite_same_mtime(p: &Path, data: &[u8]) {
    let mtime = fs::metadata(p).unwrap().modified().unwrap();
    fs::write(p, data).unwrap();
    fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(fs::metadata(p).unwrap().modified().unwrap(), mtime);
}

#[test]
fn cache_is_written_only_where_trusted() {
    let (d, repo) = repo_with(&[("a", b"1"), ("b", b"2")]);
    assert_eq!(cache_file(d.path()).exists(), cfg!(unix));
    assert!(changes(&repo).is_empty());
}

#[test]
fn same_size_same_mtime_rewrite_is_detected() {
    let (d, repo) = repo_with(&[("f.txt", b"aaaa"), ("g.txt", b"keep")]);
    assert!(changes(&repo).is_empty(), "clean, and cache warm");
    rewrite_same_mtime(&d.path().join("f.txt"), b"bbbb");
    assert_eq!(changes(&repo), [("f.txt".to_owned(), ChangeKind::Modified)]);
    let diffs = diff(&repo).unwrap();
    assert!(diffs[0].unified.as_deref().unwrap().contains("+bbbb"));
}

#[test]
fn change_right_after_save_is_detected() {
    // No settling delay: the save sees fresh timestamps and must not cache.
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), cache_opts()).unwrap();
    let p = d.path().join("f");
    fs::write(&p, b"1111").unwrap();
    save(&repo, "fresh").unwrap();
    rewrite_same_mtime(&p, b"2222");
    assert_eq!(changes(&repo), [("f".to_owned(), ChangeKind::Modified)]);
}

#[test]
fn repeated_edits_across_saves_are_detected() {
    let (d, repo) = repo_with(&[("f", b"v0")]);
    let p = d.path().join("f");
    for i in 1..=3u8 {
        rewrite_same_mtime(&p, &[b'v', b'0' + i]);
        assert_eq!(changes(&repo), [("f".to_owned(), ChangeKind::Modified)]);
        sleep(WINDOW * 3);
        save(&repo, "edit").unwrap();
        assert!(changes(&repo).is_empty());
    }
}

#[test]
fn file_replaced_by_rename_is_detected() {
    let (d, repo) = repo_with(&[("f", b"same"), ("other", b"diff")]);
    // `other` is older than any later write, has the same size, and is moved
    // over `f`: a different inode must not reuse `f`'s entry.
    fs::rename(d.path().join("other"), d.path().join("f")).unwrap();
    assert_eq!(
        changes(&repo),
        [
            ("f".to_owned(), ChangeKind::Modified),
            ("other".to_owned(), ChangeKind::Deleted),
        ]
    );
}

#[test]
fn deleted_and_recreated_file_is_detected() {
    let (d, repo) = repo_with(&[("f", b"abc")]);
    let p = d.path().join("f");
    let mtime = fs::metadata(&p).unwrap().modified().unwrap();
    fs::remove_file(&p).unwrap();
    fs::write(&p, b"xyz").unwrap();
    fs::File::options()
        .write(true)
        .open(&p)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(changes(&repo), [("f".to_owned(), ChangeKind::Modified)]);
}

#[test]
fn corrupt_truncated_or_missing_cache_is_ignored() {
    let (d, repo) = repo_with(&[("a", b"1111"), ("b", b"2222")]);
    rewrite_same_mtime(&d.path().join("a"), b"3333");
    let cf = cache_file(d.path());
    let good = fs::read(&cf).unwrap_or_default();
    let mut variants: Vec<Option<Vec<u8>>> = vec![None, Some(Vec::new()), Some(b"junk".to_vec())];
    if !good.is_empty() {
        let mut flipped = good.clone();
        let mid = flipped.len() / 2;
        flipped[mid] ^= 0x40;
        variants.push(Some(flipped));
        variants.push(Some(good[..good.len() - 1].to_vec()));
    }
    for v in variants {
        match &v {
            None => {
                let _ = fs::remove_file(&cf);
            }
            Some(bytes) => fs::write(&cf, bytes).unwrap(),
        }
        assert_eq!(changes(&repo), [("a".to_owned(), ChangeKind::Modified)]);
    }
    // A later save replaces it with a valid cache (on Unix).
    sleep(WINDOW * 3);
    save(&repo, "rebuild").unwrap();
    let rebuilt = concurv_workspace::statcache::StatCache::load(&d.path().join(".concurv"));
    assert_eq!(rebuilt.len(), if cfg!(unix) { 2 } else { 0 });
    assert!(changes(&repo).is_empty());
}

#[test]
fn status_never_writes_the_cache() {
    let (d, repo) = repo_with(&[("a", b"1")]);
    let cf = cache_file(d.path());
    let before = fs::read(&cf).ok();
    fs::write(d.path().join("a"), b"2").unwrap();
    for _ in 0..3 {
        status(&repo).unwrap();
        diff(&repo).unwrap();
    }
    assert_eq!(fs::read(&cf).ok(), before);
}

#[test]
fn no_cache_option_hashes_everything() {
    let (d, repo) = repo_with(&[("a", b"1111")]);
    rewrite_same_mtime(&d.path().join("a"), b"2222");
    let full = status_with(
        &repo,
        StatusOptions {
            use_stat_cache: false,
        },
    )
    .unwrap();
    assert_eq!(full.changes.len(), 1);
}

#[test]
fn caches_are_per_workspace() {
    let outer = tempfile::tempdir().unwrap();
    let (a, b) = (outer.path().join("a"), outer.path().join("b"));
    fs::create_dir_all(&a).unwrap();
    let repo = Repo::init(&a, cache_opts()).unwrap();
    fs::write(a.join("f"), b"aaaa").unwrap();
    sleep(WINDOW * 3);
    save(&repo, "base").unwrap();
    concurv_workspace::publish(&repo).unwrap();
    concurv_workspace::workspaces::add(&repo, &b, None).unwrap();
    let repo2 = Repo::open(&b, cache_opts()).unwrap();
    sleep(WINDOW * 3);
    save(&repo2, "warm b").unwrap();
    assert!(changes(&repo2).is_empty());
    rewrite_same_mtime(&b.join("f"), b"bbbb");
    assert!(changes(&repo).is_empty());
    assert_eq!(changes(&repo2), [("f".to_owned(), ChangeKind::Modified)]);
}

#[cfg(unix)]
#[test]
fn symlink_retarget_and_mode_change_are_detected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (d, repo) = repo_with(&[("t1", b"x"), ("t2", b"y"), ("run", b"#!/bin/sh\n")]);
    symlink("t1", d.path().join("link")).unwrap();
    sleep(WINDOW * 3);
    save(&repo, "link").unwrap();
    assert!(changes(&repo).is_empty());
    fs::remove_file(d.path().join("link")).unwrap();
    symlink("t2", d.path().join("link")).unwrap();
    fs::set_permissions(d.path().join("run"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        changes(&repo),
        [
            ("link".to_owned(), ChangeKind::Modified),
            ("run".to_owned(), ChangeKind::Modified),
        ]
    );
}

#[cfg(unix)]
#[test]
fn only_settled_files_are_cached() {
    use concurv_workspace::statcache::StatCache;
    let d = tempfile::tempdir().unwrap();
    let repo = Repo::init(d.path(), cache_opts()).unwrap();
    set_files(d.path(), &files(&[("old1", b"1"), ("old2", b"2")]));
    sleep(WINDOW * 3);
    fs::write(d.path().join("fresh"), b"3").unwrap();
    save(&repo, "mixed").unwrap();
    let cache = StatCache::load(&d.path().join(".concurv"));
    assert_eq!(cache.len(), 2, "the just-written file must not be cached");
    assert!(changes(&repo).is_empty());
}
