//! Disposable stat cache (R1.5 P0-7, ADR 0005: `docs/adr/0005-stat-cache.md`).
//!
//! Maps a workspace path plus its exact `lstat` identity to the blob id of
//! its content, so `status`/`diff` can skip rehashing unchanged files.
//!
//! Safety rules:
//! - Only `save` writes the cache, after hashing every file in full and under
//!   the workspace lock. `status`/`diff` only read it, so R1-I4 holds.
//! - The cache is **never** the authority for a snapshot: `save`, `restore`,
//!   `update`, `undo` and workspace checkout always hash file contents.
//! - Trusted only on Unix, where the inode change time (`ctime`) is set by
//!   the kernel on every content or metadata change and cannot be set by
//!   user code. On other platforms nothing is recorded or trusted.
//! - An entry is recorded only if the file's identity was identical before
//!   and after hashing (no change during the read), and both its `mtime` and
//!   `ctime` are older than the start of the save by a safety window, so a
//!   later change within the same timestamp tick cannot look unchanged
//!   (Git's "racy" problem).
//! - A lookup must match device, inode, size, mode, `mtime` and `ctime`
//!   exactly. Any doubt (missing, unreadable, truncated, corrupt,
//!   wrong version) means the cache is ignored; it is rebuilt by the next save.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use concurv_core::{BlobId, ObjectId, RandomId, RepoPath};

/// File name inside the workspace's `.concurv/`.
pub const STAT_CACHE: &str = "stat-cache";
/// Default safety window for the racy-timestamp guard.
pub const DEFAULT_RACY_WINDOW: Duration = Duration::from_secs(2);

const MAGIC: &[u8; 8] = b"CNVSTC1\n";
/// Upper bound on entries accepted when reading (defensive).
const MAX_ENTRIES: u64 = 50_000_000;

/// Exact identity of a regular file as reported by `lstat`/`fstat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatKey {
    dev: u64,
    ino: u64,
    size: u64,
    mode: u32,
    mtime_s: i64,
    mtime_ns: i64,
    ctime_s: i64,
    ctime_ns: i64,
}

impl StatKey {
    /// Identity of `meta`, or `None` where the cache is never trusted.
    #[cfg(unix)]
    pub fn of(meta: &fs::Metadata) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        Some(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mode: meta.mode(),
            mtime_s: meta.mtime(),
            mtime_ns: meta.mtime_nsec(),
            ctime_s: meta.ctime(),
            ctime_ns: meta.ctime_nsec(),
        })
    }

    /// Identity of `meta`, or `None` where the cache is never trusted.
    /// Windows exposes no kernel-maintained change time through `std`, and
    /// NTFS timestamps can all be set by user code, so a same-size,
    /// same-mtime rewrite could not be detected.
    #[cfg(not(unix))]
    pub fn of(_meta: &fs::Metadata) -> Option<Self> {
        None
    }

    /// True if both timestamps are older than `start - window`.
    pub fn settled(&self, start: SystemTime, window: Duration) -> bool {
        let Ok(since_epoch) = start.duration_since(UNIX_EPOCH) else {
            return false;
        };
        let Some(limit) = since_epoch.checked_sub(window) else {
            return false;
        };
        let limit = i128::try_from(limit.as_nanos()).unwrap_or(i128::MAX);
        let ts = |s: i64, ns: i64| i128::from(s) * 1_000_000_000 + i128::from(ns);
        ts(self.mtime_s, self.mtime_ns) < limit && ts(self.ctime_s, self.ctime_ns) < limit
    }

    #[cfg(test)]
    fn with_times(mtime_s: i64, ctime_s: i64) -> Self {
        Self {
            dev: 1,
            ino: 2,
            size: 3,
            mode: 0o100644,
            mtime_s,
            mtime_ns: 0,
            ctime_s,
            ctime_ns: 0,
        }
    }
}

/// Read side: entries loaded from the workspace's cache file.
#[derive(Debug, Default)]
pub struct StatCache {
    entries: HashMap<RepoPath, (StatKey, BlobId)>,
}

impl StatCache {
    /// Load the cache of the workspace whose `.concurv/` is `ws_meta`. Any
    /// problem yields an empty cache; nothing is ever an error.
    pub fn load(ws_meta: &Path) -> Self {
        if !cfg!(unix) {
            return Self::default();
        }
        fs::read(ws_meta.join(STAT_CACHE))
            .ok()
            .and_then(|b| decode(&b))
            .map(|entries| Self { entries })
            .unwrap_or_default()
    }

    /// The cached blob for `path`, only if `key` matches exactly.
    pub fn lookup(&self, path: &RepoPath, key: &StatKey) -> Option<BlobId> {
        match self.entries.get(path) {
            Some((k, blob)) if k == key => Some(*blob),
            _ => None,
        }
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Write side: collects trustworthy entries while `save` hashes files.
#[derive(Debug)]
pub struct CacheRecorder {
    start: SystemTime,
    window: Duration,
    entries: Vec<(RepoPath, StatKey, BlobId)>,
}

impl CacheRecorder {
    /// Start recording; `start` must be taken before the scan begins.
    pub fn new(start: SystemTime, window: Duration) -> Self {
        Self {
            start,
            window,
            entries: Vec::new(),
        }
    }

    /// Record `path` if its identity was the same at every check and its
    /// timestamps are settled. `keys` are the identities observed before
    /// opening, after opening and after reading.
    pub fn record(&mut self, path: &RepoPath, keys: [Option<StatKey>; 3], blob: BlobId) {
        let [Some(a), Some(b), Some(c)] = keys else {
            return;
        };
        if a == b && b == c && a.settled(self.start, self.window) {
            self.entries.push((path.clone(), a, blob));
        }
    }

    /// Replace the workspace's cache with the recorded entries. Best effort:
    /// the caller ignores failure, which leaves at worst a missing, stale or
    /// partial file that `load` rejects or that never matches. `tmp_dir`
    /// must be the workspace's `ws-tmp/` and the workspace lock must be held.
    pub fn persist(self, ws_meta: &Path, tmp_dir: &Path) -> std::io::Result<()> {
        if !cfg!(unix) {
            return Ok(());
        }
        let bytes = encode(&self.entries);
        let name = RandomId::generate()
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .to_hex();
        let tmp = tmp_dir.join(format!("stat-cache.{name}"));
        let result = (|| {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&bytes)?;
            drop(f);
            fs::rename(&tmp, ws_meta.join(STAT_CACHE))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

fn encode(entries: &[(RepoPath, StatKey, BlobId)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + entries.len() * 128);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (path, k, blob) in entries {
        let s = path.to_string();
        let p = s.as_bytes();
        out.extend_from_slice(&(p.len() as u32).to_le_bytes());
        out.extend_from_slice(p);
        for v in [k.dev, k.ino, k.size] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&k.mode.to_le_bytes());
        for v in [k.mtime_s, k.mtime_ns, k.ctime_s, k.ctime_ns] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(blob.object_id().as_bytes());
    }
    let sum = blake3::hash(&out);
    out.extend_from_slice(sum.as_bytes());
    out
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn arr<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    fn u32(&mut self) -> Option<u32> {
        self.arr().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.arr().map(u64::from_le_bytes)
    }
    fn i64(&mut self) -> Option<i64> {
        self.arr().map(i64::from_le_bytes)
    }
}

fn decode(bytes: &[u8]) -> Option<HashMap<RepoPath, (StatKey, BlobId)>> {
    let body_len = bytes.len().checked_sub(32)?;
    let (body, sum) = bytes.split_at(body_len);
    if blake3::hash(body).as_bytes() != sum {
        return None;
    }
    let mut r = Reader(body);
    if r.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let count = r.u64()?;
    if count > MAX_ENTRIES {
        return None;
    }
    let mut entries = HashMap::new();
    for _ in 0..count {
        let len = usize::try_from(r.u32()?).ok()?;
        let path = RepoPath::parse(std::str::from_utf8(r.take(len)?).ok()?).ok()?;
        let key = StatKey {
            dev: r.u64()?,
            ino: r.u64()?,
            size: r.u64()?,
            mode: r.u32()?,
            mtime_s: r.i64()?,
            mtime_ns: r.i64()?,
            ctime_s: r.i64()?,
            ctime_ns: r.i64()?,
        };
        let blob = BlobId::from_object_id(ObjectId::from_bytes(r.arr()?));
        entries.insert(path, (key, blob));
    }
    r.0.is_empty().then_some(entries)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn blob(n: u8) -> BlobId {
        BlobId::from_object_id(ObjectId::from_bytes([n; 32]))
    }

    #[test]
    fn racy_guard() {
        let start = UNIX_EPOCH + Duration::from_secs(1000);
        let w = Duration::from_secs(2);
        assert!(StatKey::with_times(997, 997).settled(start, w));
        assert!(
            !StatKey::with_times(998, 997).settled(start, w),
            "mtime in window"
        );
        assert!(
            !StatKey::with_times(997, 999).settled(start, w),
            "ctime in window"
        );
        assert!(
            !StatKey::with_times(997, 2000).settled(start, w),
            "future ctime"
        );
        assert!(
            !StatKey::with_times(0, 0).settled(UNIX_EPOCH, w),
            "underflow"
        );
    }

    #[test]
    fn record_requires_identical_keys() {
        let start = UNIX_EPOCH + Duration::from_secs(1000);
        let mut r = CacheRecorder::new(start, DEFAULT_RACY_WINDOW);
        let p = RepoPath::parse("a").unwrap();
        let k = StatKey::with_times(10, 10);
        let changed = StatKey::with_times(10, 11);
        r.record(&p, [Some(k), Some(k), Some(changed)], blob(1));
        r.record(&p, [Some(k), None, Some(k)], blob(1));
        r.record(&p, [Some(k), Some(k), Some(k)], blob(2));
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].2, blob(2));
    }

    #[test]
    fn encoding_round_trips_and_rejects_damage() {
        let k = StatKey::with_times(10, 11);
        let entries = vec![
            (RepoPath::parse("a/b.txt").unwrap(), k, blob(7)),
            (RepoPath::parse("ü").unwrap(), k, blob(8)),
        ];
        let bytes = encode(&entries);
        let m = decode(&bytes).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[&RepoPath::parse("ü").unwrap()], (k, blob(8)));
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 0x01;
            assert!(decode(&bad).is_none(), "bit flip at {i} accepted");
        }
        for n in 0..bytes.len() {
            assert!(decode(&bytes[..n]).is_none(), "truncation to {n} accepted");
        }
        assert!(decode(&[]).is_none());
    }
}
