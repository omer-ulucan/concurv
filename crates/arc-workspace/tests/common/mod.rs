#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Shared helpers. Every test uses its own temporary directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use arc_objects::Faults;
use arc_workspace::{Repo, RepoOptions};

pub type Files = BTreeMap<String, Vec<u8>>;

pub fn opts() -> RepoOptions {
    RepoOptions {
        faults: Faults::none(),
        now_ms: Some(1_793_491_200_000),
        author: Some("Test <test@example.com>".into()),
    }
}

pub fn opts_with(faults: Faults) -> RepoOptions {
    RepoOptions { faults, ..opts() }
}

pub fn new_repo() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(d.path(), opts()).unwrap();
    (d, r)
}

pub fn files(entries: &[(&str, &[u8])]) -> Files {
    entries
        .iter()
        .map(|(p, c)| ((*p).to_owned(), c.to_vec()))
        .collect()
}

/// Make the working copy contain exactly `files` (besides `.arc`).
pub fn set_files(root: &Path, files: &Files) {
    clear(root);
    for (p, c) in files {
        let full = root.join(p);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, c).unwrap();
    }
}

pub fn clear(root: &Path) {
    for e in fs::read_dir(root).unwrap() {
        let e = e.unwrap();
        if e.file_name() == ".arc" {
            continue;
        }
        let ft = e.file_type().unwrap();
        if ft.is_dir() {
            fs::remove_dir_all(e.path()).unwrap();
        } else {
            fs::remove_file(e.path()).unwrap();
        }
    }
}

/// Read every regular file under `root` (excluding `.arc`).
pub fn read_disk(root: &Path) -> Files {
    let mut out = Files::new();
    walk(root, "", &mut out);
    out
}

fn walk(root: &Path, rel: &str, out: &mut Files) {
    for e in fs::read_dir(root.join(rel)).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        if rel.is_empty() && name == ".arc" {
            continue;
        }
        let p = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let ft = fs::symlink_metadata(e.path()).unwrap().file_type();
        if ft.is_dir() {
            walk(root, &p, out);
        } else if ft.is_file() {
            out.insert(p, fs::read(e.path()).unwrap());
        }
    }
}

pub fn op_head(repo: &Repo) -> String {
    repo.op_head().read().unwrap().to_hex()
}
