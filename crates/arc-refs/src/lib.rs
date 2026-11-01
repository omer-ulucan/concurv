//! The repository's single mutable pointer (`.arc/op_head`) and the
//! exclusive writer lock that serializes its compare-and-swap (RFC 0002 §3.2, §4).

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use arc_core::{ArcError, ErrorCode, ObjectId, OperationId, Result};
use arc_objects::Faults;
use arc_objects::fs::write_file_atomic;

/// An exclusive OS advisory lock on `.arc/lock`. Released when dropped or
/// when the process exits for any reason (so crashes never leave it stale).
#[derive(Debug)]
pub struct RepoLock {
    _file: File,
    arc_dir: PathBuf,
}

impl RepoLock {
    /// Try to take the lock without blocking.
    pub fn acquire(arc_dir: &Path) -> Result<Self> {
        let path = arc_dir.join("lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| ArcError::io(e, "open lock file", &path))?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                arc_dir: arc_dir.to_path_buf(),
            }),
            Err(TryLockError::WouldBlock) => Err(ArcError::new(
                ErrorCode::Locked,
                "another ARC process is modifying this repository; retry when it finishes",
            )
            .with_path(&path)),
            Err(TryLockError::Error(e)) => Err(ArcError::io(e, "lock", &path)),
        }
    }

    /// The `.arc` directory this lock protects.
    pub fn arc_dir(&self) -> &Path {
        &self.arc_dir
    }
}

/// Accessor for `.arc/op_head`.
#[derive(Clone, Debug)]
pub struct OpHead {
    path: PathBuf,
    tmp: PathBuf,
    arc_dir: PathBuf,
    faults: Faults,
}

impl OpHead {
    /// Handle for the op head of the repository at `arc_dir`.
    pub fn new(arc_dir: &Path, faults: Faults) -> Self {
        Self {
            path: arc_dir.join("op_head"),
            tmp: arc_dir.join("tmp"),
            arc_dir: arc_dir.to_path_buf(),
            faults,
        }
    }

    /// Read and strictly parse the current op head.
    pub fn read(&self) -> Result<OperationId> {
        let s = match fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(ArcError::new(
                    ErrorCode::ObjectCorrupt,
                    "repository has no op_head; it is damaged (run `arc verify --full`)",
                )
                .with_path(&self.path));
            }
            Err(e) => return Err(ArcError::io(e, "read", &self.path)),
        };
        let hex = s.strip_suffix('\n').unwrap_or("");
        ObjectId::from_hex(hex)
            .map(OperationId::from_object_id)
            .map_err(|_| {
                ArcError::new(ErrorCode::ObjectCorrupt, "op_head is malformed")
                    .with_path(&self.path)
            })
    }

    /// Write the op head unconditionally. Only for `init`, which builds the
    /// repository in a private directory before it becomes visible.
    pub fn write_initial(&self, id: &OperationId) -> Result<()> {
        self.write(id)
    }

    fn write(&self, id: &OperationId) -> Result<()> {
        let content = format!("{}\n", id.to_hex());
        write_file_atomic(&self.tmp, &self.path, content.as_bytes(), &self.faults)
    }

    /// Replace the op head with `new` only if it still equals `expected`.
    /// Requires the repository lock. The rename inside is the commit point.
    pub fn compare_and_swap(
        &self,
        lock: &RepoLock,
        expected: &OperationId,
        new: &OperationId,
    ) -> Result<()> {
        if lock.arc_dir() != self.arc_dir {
            return Err(ArcError::internal("lock belongs to a different repository"));
        }
        let current = self.read()?;
        if current != *expected {
            return Err(ArcError::new(
                ErrorCode::StaleHead,
                format!("op head moved from {expected} to {current}; retry the command"),
            ));
        }
        self.write(new)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn setup() -> (tempfile::TempDir, OpHead) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("tmp")).unwrap();
        let h = OpHead::new(d.path(), Faults::none());
        (d, h)
    }

    fn id(b: u8) -> OperationId {
        OperationId::from_object_id(ObjectId::from_bytes([b; 32]))
    }

    #[test]
    fn cas_succeeds_only_on_expected() {
        let (d, h) = setup();
        h.write_initial(&id(1)).unwrap();
        let lock = RepoLock::acquire(d.path()).unwrap();
        h.compare_and_swap(&lock, &id(1), &id(2)).unwrap();
        assert_eq!(h.read().unwrap(), id(2));
        let e = h.compare_and_swap(&lock, &id(1), &id(3)).unwrap_err();
        assert_eq!(e.code, ErrorCode::StaleHead);
        assert_eq!(h.read().unwrap(), id(2));
    }

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let (d, _h) = setup();
        let l = RepoLock::acquire(d.path()).unwrap();
        assert_eq!(
            RepoLock::acquire(d.path()).unwrap_err().code,
            ErrorCode::Locked
        );
        drop(l);
        RepoLock::acquire(d.path()).unwrap();
    }

    #[test]
    fn malformed_head_is_reported() {
        let (d, h) = setup();
        fs::write(d.path().join("op_head"), "zz\n").unwrap();
        assert_eq!(h.read().unwrap_err().code, ErrorCode::ObjectCorrupt);
        fs::write(d.path().join("op_head"), id(1).to_hex()).unwrap();
        assert_eq!(h.read().unwrap_err().code, ErrorCode::ObjectCorrupt);
    }
}
