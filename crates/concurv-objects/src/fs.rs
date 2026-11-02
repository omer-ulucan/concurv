//! Durable filesystem primitives and deterministic fault injection
//! (RFC 0002 §3, §6).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use concurv_core::{ConcurvError, ErrorCode, RandomId, Result};

/// A named step in a durable write at which a failure can be injected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaultPoint {
    /// Creating a temporary file.
    TmpCreate,
    /// Writing bytes to a temporary file (supports torn writes).
    TmpWrite,
    /// `fsync` of a temporary file.
    TmpSync,
    /// Creating a directory.
    DirCreate,
    /// Atomic rename into place.
    Rename,
    /// `fsync` of a directory.
    DirSync,
}

/// All fault points, for exhaustive test iteration.
pub const ALL_FAULT_POINTS: [FaultPoint; 6] = [
    FaultPoint::TmpCreate,
    FaultPoint::TmpWrite,
    FaultPoint::TmpSync,
    FaultPoint::DirCreate,
    FaultPoint::Rename,
    FaultPoint::DirSync,
];

/// How an injected fault behaves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultMode {
    /// Return an I/O error.
    Error,
    /// For `TmpWrite`: write half of the buffer, then error. Otherwise like `Error`.
    TornWrite,
}

#[derive(Debug, Default)]
struct FaultState {
    target: Option<(FaultPoint, u64, FaultMode)>,
    hits: HashMap<FaultPoint, u64>,
    triggered: bool,
    transient: bool,
}

/// Fault-injection handle. The default handle never fails and costs one
/// branch per check. Once a fault triggers, **every later check also fails**,
/// modelling a process that died at that point.
#[derive(Clone, Debug, Default)]
pub struct Faults(Option<Arc<Mutex<FaultState>>>);

enum WriteDecision {
    Full,
    Torn,
}

fn injected(point: FaultPoint) -> io::Error {
    io::Error::other(format!("injected fault at {point:?}"))
}

impl Faults {
    /// Never fails, records nothing.
    pub fn none() -> Self {
        Self(None)
    }

    /// Never fails, but counts hits per point (to enumerate test cases).
    pub fn recording() -> Self {
        Self(Some(Arc::default()))
    }

    /// Fail the `nth` (1-based) hit of `point`.
    pub fn fail_at(point: FaultPoint, nth: u64, mode: FaultMode) -> Self {
        Self(Some(Arc::new(Mutex::new(FaultState {
            target: Some((point, nth, mode)),
            ..FaultState::default()
        }))))
    }

    /// Fail only the `nth` hit of `point`; later I/O succeeds again. Models a
    /// transient error (e.g. one file that cannot be written) in a process
    /// that keeps running, as opposed to [`Faults::fail_at`], which models a
    /// process that died.
    pub fn fail_once_at(point: FaultPoint, nth: u64, mode: FaultMode) -> Self {
        Self(Some(Arc::new(Mutex::new(FaultState {
            target: Some((point, nth, mode)),
            transient: true,
            ..FaultState::default()
        }))))
    }

    fn with<R>(&self, f: impl FnOnce(&mut FaultState) -> R) -> Option<R> {
        self.0
            .as_ref()
            .map(|m| f(&mut m.lock().unwrap_or_else(|p| p.into_inner())))
    }

    /// Number of times `point` was reached.
    pub fn hits(&self, point: FaultPoint) -> u64 {
        self.with(|s| s.hits.get(&point).copied().unwrap_or(0))
            .unwrap_or(0)
    }

    /// True once an injected fault has fired.
    pub fn triggered(&self) -> bool {
        self.with(|s| s.triggered).unwrap_or(false)
    }

    fn decide(&self, point: FaultPoint) -> io::Result<WriteDecision> {
        let r = self.with(|s| {
            if s.triggered && !s.transient {
                return Err(injected(point));
            }
            let n = s.hits.entry(point).or_insert(0);
            *n += 1;
            match s.target {
                Some((p, nth, mode)) if p == point && nth == *n => {
                    s.triggered = true;
                    match mode {
                        FaultMode::TornWrite if point == FaultPoint::TmpWrite => {
                            Ok(WriteDecision::Torn)
                        }
                        _ => Err(injected(point)),
                    }
                }
                _ => Ok(WriteDecision::Full),
            }
        });
        r.unwrap_or(Ok(WriteDecision::Full))
    }

    /// Check a fault point; returns an error if a fault is injected here.
    pub fn check(&self, point: FaultPoint) -> io::Result<()> {
        self.decide(point).map(|_| ())
    }
}

/// Write `buf` to `file` through the `TmpWrite` fault point.
pub fn write_chunk(file: &mut File, buf: &[u8], faults: &Faults) -> io::Result<()> {
    match faults.decide(FaultPoint::TmpWrite)? {
        WriteDecision::Full => file.write_all(buf),
        WriteDecision::Torn => {
            file.write_all(&buf[..buf.len() / 2])?;
            Err(injected(FaultPoint::TmpWrite))
        }
    }
}

/// `fsync` a file through the `TmpSync` fault point.
pub fn sync_file(file: &File, faults: &Faults) -> io::Result<()> {
    faults.check(FaultPoint::TmpSync)?;
    file.sync_all()
}

/// `fsync` a directory so that renames/creations inside it are durable.
///
/// On Windows the standard library cannot open a directory handle, so this
/// is a no-op beyond the fault point (documented limitation, RFC 0002 §5).
pub fn sync_dir(dir: &Path, faults: &Faults) -> Result<()> {
    faults
        .check(FaultPoint::DirSync)
        .map_err(|e| ConcurvError::io(e, "sync directory", dir))?;
    #[cfg(unix)]
    {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| ConcurvError::io(e, "sync directory", dir))?;
    }
    Ok(())
}

/// Create `dir` if missing and make its creation durable in the parent.
pub fn create_dir_durable(dir: &Path, faults: &Faults) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => return Ok(()),
        Ok(_) => {
            return Err(ConcurvError::new(
                concurv_core::ErrorCode::ObjectCorrupt,
                format!("`{}` exists but is not a directory", dir.display()),
            )
            .with_path(dir));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(ConcurvError::io(e, "inspect directory", dir)),
    }
    faults
        .check(FaultPoint::DirCreate)
        .map_err(|e| ConcurvError::io(e, "create directory", dir))?;
    match fs::create_dir(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(ConcurvError::io(e, "create directory", dir)),
    }
    if let Some(parent) = dir.parent() {
        sync_dir(parent, faults)?;
    }
    Ok(())
}

/// A uniquely named temporary file that is removed on drop unless persisted
/// (or unless a fault fired, to leave crash-like debris for recovery tests).
pub struct TempFile {
    path: PathBuf,
    file: Option<File>,
    faults: Faults,
}

impl TempFile {
    /// Create a new temp file inside `tmp_dir` with `create_new` semantics.
    pub fn create(tmp_dir: &Path, prefix: &str, faults: &Faults) -> Result<Self> {
        let rid = RandomId::generate()?;
        let path = tmp_dir.join(format!("{prefix}.{}.tmp", rid.to_hex()));
        faults
            .check(FaultPoint::TmpCreate)
            .map_err(|e| ConcurvError::io(e, "create temporary file", &path))?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| ConcurvError::io(e, "create temporary file", &path))?;
        Ok(Self {
            path,
            file: Some(file),
            faults: faults.clone(),
        })
    }

    /// Path of the temp file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write bytes (fault point `TmpWrite`).
    pub fn write(&mut self, buf: &[u8]) -> Result<()> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| ConcurvError::internal("temp file already closed"))?;
        write_chunk(file, buf, &self.faults)
            .map_err(|e| ConcurvError::io(e, "write temporary file", &self.path))
    }

    /// `fsync` and close the file (fault point `TmpSync`).
    pub fn sync(&mut self) -> Result<()> {
        let file = self
            .file
            .take()
            .ok_or_else(|| ConcurvError::internal("temp file already closed"))?;
        sync_file(&file, &self.faults).map_err(|e| ConcurvError::io(e, "sync file", &self.path))
    }

    /// Atomically rename onto `dest` and sync `dest`'s directory.
    /// The file must have been synced first.
    pub fn persist(mut self, dest: &Path) -> Result<()> {
        if self.file.is_some() {
            return Err(ConcurvError::internal("persist called before sync"));
        }
        rename_durable(&self.path, dest, &self.faults)?;
        // Prevent Drop from deleting anything.
        self.path = PathBuf::new();
        Ok(())
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        self.file.take();
        if !self.path.as_os_str().is_empty() && !self.faults.triggered() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Rename `from` to `to` (fault point `Rename`), then sync `to`'s parent.
pub fn rename_durable(from: &Path, to: &Path, faults: &Faults) -> Result<()> {
    faults
        .check(FaultPoint::Rename)
        .map_err(|e| ConcurvError::io(e, "rename into", to))?;
    rename_with_retry(from, to).map_err(|e| ConcurvError::io(e, "rename into", to))?;
    if let Some(parent) = to.parent() {
        sync_dir(parent, faults)?;
    }
    Ok(())
}

/// `fs::rename` with a short retry on Windows, where virus scanners and
/// indexers can briefly hold files open and cause sharing violations.
fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match fs::rename(from, to) {
            Err(e)
                if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied && attempt < 5 =>
            {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(10 * attempt));
            }
            other => return other,
        }
    }
}

/// Atomically replace `dest` with `bytes`: temp file in `tmp_dir`, write,
/// fsync, rename, fsync directory.
pub fn write_file_atomic(tmp_dir: &Path, dest: &Path, bytes: &[u8], faults: &Faults) -> Result<()> {
    let mut t = TempFile::create(tmp_dir, "atomic", faults)?;
    t.write(bytes)?;
    t.sync()?;
    t.persist(dest)
}

// ---------------------------------------------------------------- locks

/// An exclusive OS advisory lock on a file. Released when dropped, and by the
/// OS when the process exits for any reason, so a crash never leaves a stale
/// lock behind.
#[derive(Debug)]
pub struct FileLock {
    _file: File,
    path: PathBuf,
}

impl FileLock {
    fn open(path: &Path) -> Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| ConcurvError::io(e, "open lock file", path))
    }

    /// Take the lock if it is free; `Ok(None)` if another holder has it.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = Self::open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                _file: file,
                path: path.to_path_buf(),
            })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(ConcurvError::io(e, "lock", path)),
        }
    }

    /// Take the lock, waiting up to `timeout` with exponential backoff
    /// (5 ms doubling to 200 ms). `what` names the lock in the error message.
    pub fn acquire(path: &Path, timeout: Duration, what: &str) -> Result<Self> {
        let start = Instant::now();
        let mut delay = Duration::from_millis(5);
        loop {
            if let Some(l) = Self::try_acquire(path)? {
                return Ok(l);
            }
            let waited = start.elapsed();
            if waited >= timeout {
                return Err(ConcurvError::new(
                    ErrorCode::Locked,
                    format!(
                        "{what} is held by another concurv process (waited {} ms); retry when it finishes",
                        waited.as_millis()
                    ),
                )
                .with_path(path));
            }
            std::thread::sleep(delay.min(timeout - waited));
            delay = (delay * 2).min(Duration::from_millis(200));
        }
    }

    /// Path of the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// --------------------------------------------------------- temp sessions

/// A per-process temporary directory `tmp/s-<random>/`, guarded by the lock
/// file `tmp/s-<random>.lock` for as long as the session lives. Recovery
/// ([`clean_tmp_root`]) deletes a session only if it can take that lock,
/// which proves the owner has exited, so lock-free writers never lose
/// in-progress temp files.
///
/// Create sessions only while holding the lock that also serializes
/// [`clean_tmp_root`] for the same `tmp` root (the repository lock for the
/// store, the workspace lock for a workspace).
#[derive(Debug)]
pub struct TempSession {
    dir: PathBuf,
    lock: Option<FileLock>,
    faults: Faults,
}

impl TempSession {
    /// Create a new session under `tmp_root` (which must exist).
    pub fn create(tmp_root: &Path, faults: &Faults) -> Result<Self> {
        let name = format!("s-{}", RandomId::generate()?.to_hex());
        let lock_path = tmp_root.join(format!("{name}.lock"));
        let lock = FileLock::try_acquire(&lock_path)?.ok_or_else(|| {
            ConcurvError::internal("freshly created temp-session lock is already held")
        })?;
        let dir = tmp_root.join(&name);
        faults
            .check(FaultPoint::DirCreate)
            .map_err(|e| ConcurvError::io(e, "create temp session", &dir))?;
        fs::create_dir(&dir).map_err(|e| ConcurvError::io(e, "create temp session", &dir))?;
        Ok(Self {
            dir,
            lock: Some(lock),
            faults: faults.clone(),
        })
    }

    /// Directory for this session's temp files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TempSession {
    fn drop(&mut self) {
        if self.faults.triggered() {
            // Leave crash-like debris; recovery must handle it.
            return;
        }
        let _ = fs::remove_dir_all(&self.dir);
        if let Some(lock) = self.lock.take() {
            let p = lock.path().to_path_buf();
            drop(lock);
            let _ = fs::remove_file(p);
        }
    }
}

/// Remove `p` (file or directory tree); true if it is gone afterwards.
///
/// Best effort by design: debris removal is never needed for correctness
/// (temp names are random and nothing references them), and on Windows an
/// entry that its exiting owner is deleting at the same moment can be
/// "delete pending" and refuse to open or delete. A failed removal is left
/// for a later cleanup instead of failing the user's command.
fn remove_debris(p: &Path) -> bool {
    let r = match fs::symlink_metadata(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return true,
        Err(_) => return false,
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
    };
    match r {
        Ok(()) => true,
        Err(e) => e.kind() == io::ErrorKind::NotFound,
    }
}

/// Remove debris from `tmp_root`: loose files, and session directories whose
/// owner is gone (lock acquirable or missing). Live sessions are never
/// touched. Removal is best effort (see [`remove_debris`]). The caller must
/// hold the lock that serializes session creation for this root. Returns the
/// number of entries removed.
pub fn clean_tmp_root(tmp_root: &Path) -> Result<usize> {
    let rd = match fs::read_dir(tmp_root) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(ConcurvError::io(e, "list", tmp_root)),
    };
    let mut names = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| ConcurvError::io(e, "list", tmp_root))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let mut removed = 0;
    for name in &names {
        let p = tmp_root.join(name);
        if let Some(session) = name.strip_suffix(".lock") {
            // A session lock: if it can be taken, the owner is gone. If it
            // cannot even be opened (being deleted by its owner), skip it.
            let Ok(Some(lock)) = FileLock::try_acquire(&p) else {
                continue;
            };
            let dir_gone = remove_debris(&tmp_root.join(session));
            drop(lock);
            // Remove the lock file only once the directory is gone, so a
            // half-cleaned session is still recognized next time.
            if dir_gone && remove_debris(&p) {
                removed += 1;
            }
        } else if name.starts_with("s-") {
            // Session directory whose lock file is already gone: dead.
            if !names.contains(&format!("{name}.lock")) && remove_debris(&p) {
                removed += 1;
            }
        } else if remove_debris(&p) {
            // Loose temp files are only created under the guarding lock,
            // which the caller now holds, so they belong to a dead command.
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faults_are_sticky_and_counted() {
        let f = Faults::fail_at(FaultPoint::Rename, 2, FaultMode::Error);
        f.check(FaultPoint::Rename).unwrap();
        f.check(FaultPoint::TmpSync).unwrap();
        assert!(f.check(FaultPoint::Rename).is_err());
        assert!(f.triggered());
        assert!(f.check(FaultPoint::TmpSync).is_err(), "must stay failed");
        assert_eq!(f.hits(FaultPoint::Rename), 2);
    }

    #[test]
    fn atomic_write_and_torn_write() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("x");
        write_file_atomic(d.path(), &dest, b"hello", &Faults::none()).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"hello");

        let f = Faults::fail_at(FaultPoint::TmpWrite, 1, FaultMode::TornWrite);
        assert!(write_file_atomic(d.path(), &dest, b"world!", &f).is_err());
        // Destination untouched; torn temp debris left behind like a crash.
        assert_eq!(fs::read(&dest).unwrap(), b"hello");
        let debris: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert_eq!(debris.len(), 1);
        assert_eq!(fs::read(debris[0].path()).unwrap(), b"wor");
    }
}
