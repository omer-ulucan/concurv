//! The repository's single mutable pointer (`.concurv/op_head`) and the
//! exclusive writer lock that serializes its compare-and-swap (RFC 0002 §3.2, §4).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use concurv_core::{ConcurvError, ErrorCode, ObjectId, OperationId, Result};
use concurv_objects::Faults;
use concurv_objects::fs::{FileLock, write_file_atomic};

/// The repository lock: an exclusive OS advisory lock on the store's
/// `.concurv/lock`, held only for the commit step (RFC 0004 §5). Released when
/// dropped or when the process exits for any reason (so crashes never leave
/// it stale).
#[derive(Debug)]
pub struct RepoLock {
    _lock: FileLock,
    meta_dir: PathBuf,
}

impl RepoLock {
    /// Take the lock without waiting.
    pub fn acquire(meta_dir: &Path) -> Result<Self> {
        Self::acquire_timeout(meta_dir, Duration::ZERO)
    }

    /// Take the lock, waiting up to `timeout` for another holder to finish.
    pub fn acquire_timeout(meta_dir: &Path, timeout: Duration) -> Result<Self> {
        let lock = FileLock::acquire(&meta_dir.join("lock"), timeout, "the repository lock")?;
        Ok(Self {
            _lock: lock,
            meta_dir: meta_dir.to_path_buf(),
        })
    }

    /// The `.concurv` directory this lock protects.
    pub fn meta_dir(&self) -> &Path {
        &self.meta_dir
    }
}

/// Accessor for `.concurv/op_head`.
#[derive(Clone, Debug)]
pub struct OpHead {
    path: PathBuf,
    tmp: PathBuf,
    meta_dir: PathBuf,
    faults: Faults,
}

impl OpHead {
    /// Handle for the op head of the repository at `meta_dir`.
    pub fn new(meta_dir: &Path, faults: Faults) -> Self {
        Self {
            path: meta_dir.join("op_head"),
            tmp: meta_dir.join("tmp"),
            meta_dir: meta_dir.to_path_buf(),
            faults,
        }
    }

    /// Read and strictly parse the current op head.
    pub fn read(&self) -> Result<OperationId> {
        let s = match fs::read_to_string(&self.path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(ConcurvError::new(
                    ErrorCode::ObjectCorrupt,
                    "repository has no op_head; it is damaged (run `concurv verify --full`)",
                )
                .with_path(&self.path));
            }
            Err(e) => return Err(ConcurvError::io(e, "read", &self.path)),
        };
        let hex = s.strip_suffix('\n').unwrap_or("");
        ObjectId::from_hex(hex)
            .map(OperationId::from_object_id)
            .map_err(|_| {
                ConcurvError::new(ErrorCode::ObjectCorrupt, "op_head is malformed")
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
        if lock.meta_dir() != self.meta_dir {
            return Err(ConcurvError::internal(
                "lock belongs to a different repository",
            ));
        }
        let current = self.read()?;
        if current != *expected {
            return Err(ConcurvError::new(
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
