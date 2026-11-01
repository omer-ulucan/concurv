//! Operation journal: committing operations through the op-head CAS,
//! walking operation history, and crash recovery of transient files.

use std::fs;
use std::io;
use std::path::Path;

use concurv_core::{ConcurvError, ErrorCode, OperationId, Result};
use concurv_objects::{ObjectStore, Operation};
use concurv_refs::{OpHead, RepoLock};

/// The operation log of one repository.
#[derive(Clone, Debug)]
pub struct OpLog {
    store: ObjectStore,
    head: OpHead,
}

impl OpLog {
    /// Create from an object store and op-head handle of the same repository.
    pub fn new(store: ObjectStore, head: OpHead) -> Self {
        Self { store, head }
    }

    /// Current op-head id and verified operation.
    pub fn head(&self) -> Result<(OperationId, Operation)> {
        let id = self.head.read()?;
        let op = self.store.read_operation(&id).map_err(|mut e| {
            if e.code == ErrorCode::ObjectMissing {
                e.message = format!("op_head names a missing operation: {}", e.message);
            }
            e
        })?;
        Ok((id, op))
    }

    /// Durably store `op` and atomically make it the op head, provided the
    /// head is still `expected` and `op`'s first parent is `expected`.
    ///
    /// Callers must already have durably written every object referenced by
    /// `op.view`; as defence in depth this re-verifies each referenced
    /// snapshot object (and backup) before the commit point.
    pub fn commit(
        &self,
        lock: &RepoLock,
        expected: &OperationId,
        op: &Operation,
    ) -> Result<OperationId> {
        if op.parents.first() != Some(expected) {
            return Err(ConcurvError::internal(
                "operation's first parent must be the expected op head",
            ));
        }
        let referenced = op
            .view
            .branches
            .values()
            .chain(op.view.workspaces.values().filter_map(|w| w.head.as_ref()))
            .chain(op.backup.iter());
        for snap in referenced {
            self.store.read_snapshot(snap)?;
        }
        let id = self.store.write_operation(op)?;
        self.head.compare_and_swap(lock, expected, &id)?;
        Ok(id)
    }

    /// Walk first-parent operation history from the head, newest first.
    pub fn walk(&self, limit: usize) -> Result<Vec<(OperationId, Operation)>> {
        let (mut id, mut op) = self.head()?;
        let mut out = Vec::new();
        loop {
            let next = op.parents.first().copied();
            out.push((id, op));
            if out.len() >= limit {
                break;
            }
            match next {
                Some(p) => {
                    id = p;
                    op = self.store.read_operation(&p)?;
                }
                None => break,
            }
        }
        Ok(out)
    }
}

/// Delete everything in `.concurv/tmp`. Must only be called while holding the
/// repository lock: temp files are never referenced by committed state, so
/// any that exist belong to an interrupted transaction.
pub fn clean_tmp(_lock: &RepoLock, tmp_dir: &Path) -> Result<usize> {
    let mut n = 0;
    let rd = match fs::read_dir(tmp_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(ConcurvError::io(e, "list", tmp_dir)),
    };
    for entry in rd {
        let entry = entry.map_err(|e| ConcurvError::io(e, "list", tmp_dir))?;
        let p = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| ConcurvError::io(e, "inspect", &p))?;
        let r = if ft.is_dir() {
            fs::remove_dir_all(&p)
        } else {
            fs::remove_file(&p)
        };
        r.map_err(|e| ConcurvError::io(e, "remove stale temporary file", &p))?;
        n += 1;
    }
    Ok(n)
}
