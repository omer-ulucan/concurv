//! The snapshot ↔ Git commit map (`.concurv/git-map`).
//!
//! The map is a cache, not history. Export is deterministic (the same
//! snapshot always becomes the same Git commit), so a lost or stale map only
//! costs re-exporting; it can never produce different commits.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;

use concurv_core::{ConcurvError, ErrorCode, ObjectId, Result, SnapshotId};
use concurv_objects::fs::write_file_atomic;
use concurv_workspace::Repo;

/// Snapshot → Git commit id (hex, SHA-1 or SHA-256).
#[derive(Debug, Default, Clone)]
pub struct GitMap {
    entries: BTreeMap<SnapshotId, String>,
}

fn map_path(repo: &Repo) -> PathBuf {
    repo.store_dir().join("git-map")
}

fn is_git_oid(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl GitMap {
    /// Load the map (empty if absent). Malformed lines are an error, not
    /// silently skipped.
    pub fn load(repo: &Repo) -> Result<Self> {
        let p = map_path(repo);
        let s = match fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConcurvError::io(e, "read", &p)),
        };
        let mut entries = BTreeMap::new();
        for (n, line) in s.lines().enumerate() {
            let bad = || {
                ConcurvError::new(
                    ErrorCode::ObjectCorrupt,
                    format!(
                        "malformed line {} in .concurv/git-map (delete the file to rebuild it)",
                        n + 1
                    ),
                )
                .with_path(&p)
            };
            let (snap, oid) = line.split_once(' ').ok_or_else(bad)?;
            let snap = SnapshotId::from_object_id(ObjectId::from_hex(snap).map_err(|_| bad())?);
            if !is_git_oid(oid) {
                return Err(bad());
            }
            entries.insert(snap, oid.to_owned());
        }
        Ok(Self { entries })
    }

    /// Persist atomically.
    pub fn save(&self, repo: &Repo) -> Result<()> {
        let mut s = String::new();
        for (snap, oid) in &self.entries {
            s.push_str(&format!("{snap} {oid}\n"));
        }
        let _lock = repo.lock()?;
        write_file_atomic(
            repo.store().tmp_dir(),
            &map_path(repo),
            s.as_bytes(),
            repo.store().faults(),
        )
    }

    /// Git commit for a snapshot.
    pub fn get(&self, s: &SnapshotId) -> Option<&str> {
        self.entries.get(s).map(String::as_str)
    }

    /// Record a pair.
    pub fn insert(&mut self, s: SnapshotId, oid: String) {
        self.entries.insert(s, oid);
    }

    /// All pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&SnapshotId, &String)> {
        self.entries.iter()
    }
}
