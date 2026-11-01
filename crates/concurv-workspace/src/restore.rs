//! `concurv restore`: safely make the working copy equal to a snapshot
//! (RFC 0002 §3.4). History is not rewritten: the workspace head and branch
//! stay where they are; saving afterwards records a new forward snapshot.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use concurv_core::path::PlatformRules;
use concurv_core::{ConcurvError, ErrorCode, ObjectId, OperationId, RepoPath, Result, SnapshotId};
use concurv_objects::fs::{
    TempFile, create_dir_durable, rename_durable, sync_dir, write_file_atomic,
};
use concurv_objects::{EntryKind, ObjectKind, Operation};
use concurv_refs::RepoLock;

use crate::repo::Repo;
use crate::save::store_working_copy;
use crate::status::diff_flat;
use crate::worktree::{
    FileState, FlatTree, Present, ScanMode, current_state, flatten, scan, stat_path,
};

/// Options for restore.
#[derive(Clone, Copy, Debug, Default)]
pub struct RestoreOptions {
    /// Overwrite a dirty working copy after saving it as a backup snapshot.
    pub force: bool,
}

/// Result of a completed restore.
#[derive(Debug)]
pub struct RestoreOutcome {
    /// Snapshot now materialized in the working copy.
    pub target: SnapshotId,
    /// Backup of the previous dirty working copy, if one was needed.
    pub backup: Option<SnapshotId>,
    /// Files written.
    pub written: usize,
    /// Files deleted.
    pub deleted: usize,
    /// Operation recording the restore.
    pub operation: OperationId,
}

/// Contents of `.concurv/restore-in-progress`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreMarker {
    /// Snapshot being restored.
    pub target: SnapshotId,
    /// Backup taken before modifying files.
    pub backup: Option<SnapshotId>,
}

fn marker_bytes(m: &RestoreMarker) -> String {
    format!(
        "concurv-restore 1\ntarget {}\nbackup {}\n",
        m.target,
        m.backup.map_or_else(|| "-".to_owned(), |b| b.to_hex())
    )
}

/// Read the interrupted-restore marker, if present.
pub fn read_marker(repo: &Repo) -> Result<Option<RestoreMarker>> {
    let p = repo.restore_marker();
    let s = match fs::read_to_string(&p) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConcurvError::io(e, "read", &p)),
    };
    let bad =
        || ConcurvError::new(ErrorCode::ObjectCorrupt, "malformed restore marker").with_path(&p);
    let mut lines = s.lines();
    if lines.next() != Some("concurv-restore 1") {
        return Err(bad());
    }
    let target = lines
        .next()
        .and_then(|l| l.strip_prefix("target "))
        .and_then(|h| ObjectId::from_hex(h).ok())
        .map(SnapshotId::from_object_id)
        .ok_or_else(bad)?;
    let backup = match lines.next().and_then(|l| l.strip_prefix("backup ")) {
        Some("-") => None,
        Some(h) => Some(SnapshotId::from_object_id(
            ObjectId::from_hex(h).map_err(|_| bad())?,
        )),
        None => return Err(bad()),
    };
    Ok(Some(RestoreMarker { target, backup }))
}

/// Remove the marker durably (no-op if absent).
pub(crate) fn clear_marker(repo: &Repo, _lock: &RepoLock) -> Result<()> {
    let p = repo.restore_marker();
    match fs::remove_file(&p) {
        Ok(()) => sync_dir(repo.meta_dir(), repo.store().faults()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ConcurvError::io(e, "remove", &p)),
    }
}

/// Restore the working copy to `target`.
pub fn restore(repo: &Repo, target: SnapshotId, opts: RestoreOptions) -> Result<RestoreOutcome> {
    let lock = repo.lock()?;
    let store = repo.store();
    let (op_id, op, ws) = repo.current()?;

    // 1. Load and fully verify the target before touching the working copy.
    let snap = store.read_snapshot(&target)?;
    let target_files = flatten(store, &snap.tree, Some(PlatformRules::current()))?;
    for (path, st) in &target_files {
        if cfg!(not(unix)) && st.kind == EntryKind::Symlink {
            return Err(ConcurvError::new(
                ErrorCode::UnsupportedFileType,
                format!(
                    "snapshot contains symlink `{path}`, which concurv cannot create on this platform"
                ),
            ));
        }
        let (kind, _) = store.verify(&st.blob.object_id())?;
        if kind != ObjectKind::Blob {
            return Err(ConcurvError::new(
                ErrorCode::WrongObjectKind,
                format!("`{path}` refers to a {} instead of a blob", kind.name()),
            ));
        }
    }

    // 2. Refuse to discard unsaved work unless forced; forced → backup first.
    let head_files = repo.head_files(ws.head.as_ref())?;
    let working = scan(repo.root(), ScanMode::HashOnly, &head_files)?;
    let dirty = diff_flat(&head_files, &working);
    let mut backup = None;
    if !dirty.is_empty() {
        if !opts.force {
            let list: Vec<String> = dirty
                .iter()
                .take(10)
                .map(|c| format!("  {} {}", c.kind.as_str(), c.path))
                .collect();
            let more = if dirty.len() > 10 {
                format!("\n  … and {} more", dirty.len() - 10)
            } else {
                String::new()
            };
            return Err(ConcurvError::new(
                ErrorCode::DirtyWorkspace,
                format!(
                    "working copy has {} unsaved change(s); nothing was modified.\n{}{more}\n\
                     Save them first (`concurv save -m ...`) or pass --force to keep a backup snapshot and overwrite",
                    dirty.len(),
                    list.join("\n")
                ),
            ));
        }
        let msg = format!("backup of working copy before restoring {target}");
        let (stored, b) = store_working_copy(repo, &lock, ws.head, &head_files, &msg)?;
        if stored != working {
            return Err(ConcurvError::new(
                ErrorCode::ConcurrentModification,
                "working copy changed while preparing the restore; nothing was modified, retry",
            ));
        }
        backup = Some(b);
    }

    // 3. Durable marker, then apply.
    let marker = RestoreMarker { target, backup };
    write_file_atomic(
        store.tmp_dir(),
        &repo.restore_marker(),
        marker_bytes(&marker).as_bytes(),
        store.faults(),
    )?;
    let (written, deleted) = apply(repo, &working, &target_files, &head_files)?;

    // 4. Confirm the result before recording it.
    let after = scan(repo.root(), ScanMode::HashOnly, &target_files)?;
    if after != target_files {
        return Err(ConcurvError::new(
            ErrorCode::ConcurrentModification,
            "working copy does not match the target after restore (modified concurrently?); \
             re-run the restore",
        ));
    }

    let new_op = Operation {
        parents: vec![op_id],
        timestamp_ms: repo.now_ms(),
        device_id: repo.device_id(),
        workspace_id: Some(repo.workspace_id()),
        kind: "restore".into(),
        description: format!("restore working copy to {target}"),
        backup,
        view: op.view.clone(),
    };
    let operation = repo.oplog().commit(&lock, &op_id, &new_op)?;
    clear_marker(repo, &lock)?;
    Ok(RestoreOutcome {
        target,
        backup,
        written,
        deleted,
        operation,
    })
}

fn changed_underneath(path: &RepoPath) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::ConcurrentModification,
        format!(
            "`{path}` changed during the restore; stopped before touching it. Re-run the restore"
        ),
    )
}

fn apply(
    repo: &Repo,
    working: &FlatTree,
    target: &FlatTree,
    head: &FlatTree,
) -> Result<(usize, usize)> {
    let root = repo.root();
    let faults = repo.store().faults();

    // Deletions, deepest paths first.
    let mut deletions: Vec<(&RepoPath, &FileState)> = working
        .iter()
        .filter(|(p, _)| !target.contains_key(*p))
        .collect();
    deletions.sort_by(|a, b| {
        b.0.components()
            .len()
            .cmp(&a.0.components().len())
            .then(b.0.cmp(a.0))
    });
    for (path, expected) in &deletions {
        if current_state(root, path, head)? != Some(**expected) {
            return Err(changed_underneath(path));
        }
        let native = path.to_native(root);
        fs::remove_file(&native).map_err(|e| ConcurvError::io(e, "remove", &native))?;
        prune_empty_parents(root, path, faults)?;
    }

    // Writes in path order.
    let mut written = 0;
    for (path, want) in target {
        let have = working.get(path);
        if have == Some(want) {
            continue;
        }
        ensure_parents(root, path, faults)?;
        let native = path.to_native(root);
        if stat_path(&native)? == Present::Dir {
            return Err(ConcurvError::new(
                ErrorCode::ConcurrentModification,
                format!(
                    "a directory is in the way of `{path}` (contains unversioned content?); nothing further changed"
                ),
            ));
        }
        if current_state(root, path, head)? != have.copied() {
            return Err(changed_underneath(path));
        }
        materialize(repo, path, want, &native)?;
        written += 1;
    }
    Ok((written, deletions.len()))
}

fn prune_empty_parents(
    root: &Path,
    path: &RepoPath,
    faults: &concurv_objects::Faults,
) -> Result<()> {
    let comps = path.components();
    for depth in (1..comps.len()).rev() {
        let dir = RepoPath::parse(&comps[..depth].join("/"))?.to_native(root);
        let empty = fs::read_dir(&dir)
            .map_err(|e| ConcurvError::io(e, "read directory", &dir))?
            .next()
            .is_none();
        if !empty {
            break;
        }
        fs::remove_dir(&dir).map_err(|e| ConcurvError::io(e, "remove directory", &dir))?;
        if let Some(parent) = dir.parent() {
            sync_dir(parent, faults)?;
        }
    }
    Ok(())
}

/// Create missing parent directories, refusing to traverse symlinks or files.
fn ensure_parents(root: &Path, path: &RepoPath, faults: &concurv_objects::Faults) -> Result<()> {
    let comps = path.components();
    let mut cur = root.to_path_buf();
    for c in &comps[..comps.len().saturating_sub(1)] {
        cur.push(c);
        match stat_path(&cur)? {
            Present::Dir => {}
            Present::Missing => create_dir_durable(&cur, faults)?,
            Present::Symlink(_) => {
                return Err(ConcurvError::new(
                    ErrorCode::InvalidPath,
                    format!("refusing to write through symlink `{}`", cur.display()),
                )
                .with_path(&cur));
            }
            Present::File { .. } => {
                return Err(ConcurvError::new(
                    ErrorCode::ConcurrentModification,
                    format!(
                        "`{}` appeared as a file where a directory is needed",
                        cur.display()
                    ),
                )
                .with_path(&cur));
            }
        }
    }
    Ok(())
}

struct TempWriter<'a>(&'a mut TempFile);

impl Write for TempWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf).map_err(io::Error::other)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn materialize(repo: &Repo, path: &RepoPath, want: &FileState, native: &Path) -> Result<()> {
    let store = repo.store();
    match want.kind {
        EntryKind::File | EntryKind::Executable => {
            let mut t = TempFile::create(store.tmp_dir(), "restore", store.faults())?;
            store.read_blob_to(&want.blob, &mut TempWriter(&mut t))?;
            set_mode(t.path(), want.kind == EntryKind::Executable)?;
            t.sync()?;
            t.persist(native)
        }
        EntryKind::Symlink => make_symlink(repo, path, want, native),
        EntryKind::Tree => Err(ConcurvError::internal("tree entry in flat file map")),
    }
}

#[cfg(unix)]
fn set_mode(p: &Path, exec: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if exec { 0o755 } else { 0o644 };
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
        .map_err(|e| ConcurvError::io(e, "set permissions", p))
}

#[cfg(not(unix))]
fn set_mode(_p: &Path, _exec: bool) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_symlink(repo: &Repo, _path: &RepoPath, want: &FileState, native: &Path) -> Result<()> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let store = repo.store();
    let target = store.read_blob(&want.blob)?;
    if target.is_empty() || target.contains(&0) {
        return Err(ConcurvError::new(
            ErrorCode::ObjectCorrupt,
            "invalid symlink target",
        ));
    }
    let tmp = store.tmp_dir().join(format!(
        "link.{}.tmp",
        concurv_core::RandomId::generate()?.to_hex()
    ));
    std::os::unix::fs::symlink(OsStr::from_bytes(&target), &tmp)
        .map_err(|e| ConcurvError::io(e, "create symlink", &tmp))?;
    rename_durable(&tmp, native, store.faults())
}

#[cfg(not(unix))]
fn make_symlink(_repo: &Repo, path: &RepoPath, _want: &FileState, _native: &Path) -> Result<()> {
    let _ = rename_durable;
    Err(ConcurvError::new(
        ErrorCode::UnsupportedFileType,
        format!("cannot create symlink `{path}` on this platform"),
    ))
}
