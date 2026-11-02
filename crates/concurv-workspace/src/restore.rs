//! `concurv restore`: safely make the working copy equal to a snapshot
//! (RFC 0002 §3.4). History is not rewritten: the workspace head and branch
//! stay where they are; saving afterwards records a new forward snapshot.
//!
//! The same materialization core ([`checkout`]) is used by `update` and
//! `workspace add`, which additionally move the workspace head.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use concurv_core::path::PlatformRules;
use concurv_core::{ConcurvError, ErrorCode, ObjectId, OperationId, RepoPath, Result, SnapshotId};
use concurv_objects::fs::{
    TempFile, create_dir_durable, rename_durable, sync_dir, write_file_atomic,
};
use concurv_objects::{EntryKind, ObjectKind, Operation};

use crate::repo::{Repo, WorkspaceLock};
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

/// Result of a completed restore (or checkout).
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
    /// For a branch switch (ADR 0007): the workspace head before it, i.e. the
    /// state a rollback returns to.
    pub origin: Option<SnapshotId>,
    /// For a branch switch: the branch being switched to.
    pub branch: Option<String>,
}

fn marker_bytes(m: &RestoreMarker) -> String {
    let mut s = format!(
        "concurv-restore 1\ntarget {}\nbackup {}\n",
        m.target,
        m.backup.map_or_else(|| "-".to_owned(), |b| b.to_hex())
    );
    if let (Some(o), Some(b)) = (m.origin, &m.branch) {
        s.push_str(&format!("origin {o}\nbranch {b}\n"));
    }
    s
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
    // Optional switch lines (absent in markers of restores and older builds).
    let origin = match lines.next().and_then(|l| l.strip_prefix("origin ")) {
        Some(h) => Some(SnapshotId::from_object_id(
            ObjectId::from_hex(h).map_err(|_| bad())?,
        )),
        None => None,
    };
    let branch = match (origin, lines.next().and_then(|l| l.strip_prefix("branch "))) {
        (Some(_), Some(b)) => Some(b.to_owned()),
        (Some(_), None) => return Err(bad()),
        (None, _) => None,
    };
    Ok(Some(RestoreMarker {
        target,
        backup,
        origin,
        branch,
    }))
}

/// Remove the marker durably (no-op if absent). Requires the workspace lock.
pub(crate) fn clear_marker(repo: &Repo, _lock: &WorkspaceLock) -> Result<()> {
    let p = repo.restore_marker();
    match fs::remove_file(&p) {
        Ok(()) => sync_dir(repo.workspace_meta_dir(), repo.store().faults()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ConcurvError::io(e, "remove", &p)),
    }
}

/// Restore the working copy to `target`. History is unchanged.
pub fn restore(repo: &Repo, target: SnapshotId, opts: RestoreOptions) -> Result<RestoreOutcome> {
    let ws_lock = repo.workspace_lock()?;
    checkout(
        repo,
        &ws_lock,
        target,
        Checkout {
            force: opts.force,
            set_head: false,
            kind: "restore",
            description: format!("restore working copy to {target}"),
            expected: None,
            undoes: None,
            set_merge: None,
            set_branch: None,
            transactional: false,
        },
    )
}

/// Parameters of a checkout.
pub(crate) struct Checkout {
    /// Allow overwriting a dirty working copy (after a backup snapshot).
    pub force: bool,
    /// Also set this workspace's head to the target in the same operation.
    pub set_head: bool,
    /// Operation kind recorded.
    pub kind: &'static str,
    /// Operation description recorded.
    pub description: String,
    /// Snapshot the working copy must currently equal (default: the head).
    pub expected: Option<SnapshotId>,
    /// Set this workspace's pending merge in the same operation (`Some(None)`
    /// clears it).
    pub set_merge: Option<Option<concurv_core::ConflictId>>,
    /// For an `undo`: the operation being reversed.
    pub undoes: Option<OperationId>,
    /// Set the branch this workspace tracks in the same operation (ADR 0007).
    pub set_branch: Option<Option<String>>,
    /// Branch-switch semantics (ADR 0007 §3): untracked files that do not
    /// collide with the target stay in place; colliding ones are refused;
    /// a failure before the commit point rolls the working copy back.
    pub transactional: bool,
}

/// Every proper directory prefix of the paths in `files`.
fn dir_prefixes(files: &FlatTree) -> Result<std::collections::BTreeSet<RepoPath>> {
    let mut out = std::collections::BTreeSet::new();
    for p in files.keys() {
        let comps = p.components();
        for depth in 1..comps.len() {
            out.insert(RepoPath::parse(&comps[..depth].join("/"))?);
        }
    }
    Ok(out)
}

/// Untracked paths of `working` (not in `expected`) that writing `target`
/// would overwrite or need to remove: a different file at a target path, a
/// file where the target needs a directory, or a file under a path that the
/// target makes a file. Also returns `target` extended with the untracked
/// paths that can stay as they are.
fn untracked_plan(
    working: &FlatTree,
    expected: &FlatTree,
    target: &FlatTree,
) -> Result<(Vec<RepoPath>, FlatTree)> {
    let dirs = dir_prefixes(target)?;
    let mut collisions = Vec::new();
    let mut effective = target.clone();
    for (p, st) in working {
        if expected.contains_key(p) {
            continue;
        }
        match target.get(p) {
            Some(t) if t == st => {} // already the target content
            Some(_) => collisions.push(p.clone()),
            None => {
                let comps = p.components();
                let file_above = (1..comps.len()).any(|d| {
                    RepoPath::parse(&comps[..d].join("/")).is_ok_and(|a| target.contains_key(&a))
                });
                if dirs.contains(p) || file_above {
                    collisions.push(p.clone());
                } else {
                    effective.insert(p.clone(), *st);
                }
            }
        }
    }
    Ok((collisions, effective))
}

/// Materialize `target` in this workspace (RFC 0002 §3.4), then commit one
/// operation. The caller holds the workspace lock; the repository lock is
/// taken only for short critical sections (temp session, commit).
///
/// A path whose current content already equals the target is never counted
/// as unsaved work: overwriting it loses nothing. This makes an interrupted
/// checkout resumable by running it again.
pub(crate) fn checkout(
    repo: &Repo,
    ws_lock: &WorkspaceLock,
    target: SnapshotId,
    how: Checkout,
) -> Result<RestoreOutcome> {
    let store = repo.store();
    let (_, _, ws) = repo.current()?;

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
    // Unsaved work is measured against what the working copy is expected to
    // hold: normally the head; for undoing a restore, the restored snapshot.
    let expected_files = match how.expected {
        Some(e) => repo.snapshot_files(&e)?,
        None => head_files.clone(),
    };
    let effective = if how.transactional {
        let (collisions, effective) = untracked_plan(&working, &expected_files, &target_files)?;
        if !collisions.is_empty() {
            let list: Vec<String> = collisions
                .iter()
                .take(10)
                .map(|p| format!("  {p}"))
                .collect();
            return Err(ConcurvError::new(
                ErrorCode::DirtyWorkspace,
                format!(
                    "the target would overwrite or remove {} untracked path(s); nothing was modified.\n{}\n\
                     Save, move or delete them first",
                    collisions.len(),
                    list.join("\n")
                ),
            ));
        }
        effective
    } else {
        target_files.clone()
    };
    // Ignored files are not part of `working`, so neither the dirty check nor
    // a `--force` backup covers them: never overwrite one (R1.5 P0-4).
    let collisions = ignored_collisions(repo.root(), &working, &effective, &head_files)?;
    if !collisions.is_empty() {
        let list: Vec<String> = collisions
            .iter()
            .take(10)
            .map(|p| format!("  {p}"))
            .collect();
        return Err(ConcurvError::new(
            ErrorCode::DirtyWorkspace,
            format!(
                "the target would overwrite {} ignored, untracked path(s) that concurv does not version \
                 (and so cannot back up); nothing was modified.\n{}\n\
                 Move or delete them first (this refusal also applies with --force)",
                collisions.len(),
                list.join("\n")
            ),
        ));
    }
    // A change whose current state is already the target state loses
    // nothing (it is not overwritten), e.g. after an interrupted checkout.
    let dirty: Vec<_> = diff_flat(&expected_files, &working)
        .into_iter()
        .filter(|c| working.get(&c.path) != effective.get(&c.path))
        .collect();
    let mut backup = None;
    if !dirty.is_empty() {
        if !how.force {
            return Err(dirty_error(&dirty));
        }
        let writer = repo.store_writer()?;
        let msg = format!("backup of working copy before restoring {target}");
        let (stored, b) = store_working_copy(
            repo,
            writer.store(),
            ws.head.into_iter().collect(),
            &head_files,
            &msg,
            None,
        )?;
        if stored != working {
            return Err(ConcurvError::new(
                ErrorCode::ConcurrentModification,
                "working copy changed while preparing the restore; nothing was modified, retry",
            ));
        }
        backup = Some(b);
    }

    // 3. Durable marker, then apply, using a workspace-local temp session.
    let session = repo.workspace_session(ws_lock)?;
    let wanted_branch = how.set_branch.clone().flatten();
    let marker = RestoreMarker {
        target,
        backup,
        origin: if how.transactional { ws.head } else { None },
        branch: if how.transactional {
            wanted_branch.clone()
        } else {
            None
        },
    };
    if let Err(e) = write_file_atomic(
        session.dir(),
        &repo.restore_marker(),
        marker_bytes(&marker).as_bytes(),
        store.faults(),
    ) {
        // No file was touched yet. A transactional checkout also removes a
        // marker that may have reached the disk, so nothing claims an
        // interrupted switch (best effort: a leftover one is harmless).
        if how.transactional {
            let _ = clear_marker(repo, ws_lock);
        }
        return Err(e);
    }
    let rollback = |err: ConcurvError| -> ConcurvError {
        if !how.transactional {
            return err;
        }
        roll_back(
            repo,
            ws_lock,
            session.dir(),
            &working,
            &effective,
            &head_files,
            err,
        )
    };
    let applied = apply(session.dir(), repo, &working, &effective, &head_files).and_then(|wd| {
        // 4. Confirm the result before recording it.
        let after = scan(repo.root(), ScanMode::HashOnly, &target_files)?;
        if after != effective {
            return Err(ConcurvError::new(
                ErrorCode::ConcurrentModification,
                "working copy does not match the target after restore (modified concurrently?); \
                 re-run the restore",
            ));
        }
        Ok(wd)
    });
    let (written, deleted) = match applied {
        Ok(wd) => wd,
        Err(e) => return Err(rollback(e)),
    };

    // 5. Commit against the current view.
    let committed = (|| -> Result<OperationId> {
        let lock = repo.lock()?;
        let (op_id, op, current_ws) = repo.current()?;
        if current_ws.head != ws.head {
            return Err(ConcurvError::new(
                ErrorCode::StaleHead,
                "this workspace's head changed during the restore; re-run it",
            ));
        }
        let mut view = op.view.clone();
        if let Some(w) = view.workspaces.get_mut(&repo.workspace_id()) {
            if how.set_head {
                w.head = Some(target);
            }
            if let Some(m) = how.set_merge {
                w.merge = m;
            }
            if let Some(b) = how.set_branch.clone() {
                w.branch = b;
            }
        }
        let new_op = Operation {
            parents: vec![op_id],
            timestamp_ms: repo.now_ms(),
            device_id: repo.device_id(),
            workspace_id: Some(repo.workspace_id()),
            kind: how.kind.into(),
            description: how.description.clone(),
            backup,
            undoes: how.undoes,
            checked_out: Some(target),
            view,
        };
        repo.oplog().commit(&lock, &op_id, &new_op)
    })();
    let operation = match committed {
        Ok(op) => op,
        Err(e) => {
            // Roll back only if the commit point was not reached. With the
            // workspace lock held, nobody else can set our entry to the
            // target state.
            let reached = how.transactional
                && repo.current().is_ok_and(|(_, _, now)| {
                    now.head == Some(target) && now.branch == wanted_branch
                });
            return Err(if reached { e } else { rollback(e) });
        }
    };
    clear_marker(repo, ws_lock)?;
    Ok(RestoreOutcome {
        target,
        backup,
        written,
        deleted,
        operation,
    })
}

/// Return the working copy to `original` after a failed transactional
/// checkout. Only content the checkout itself wrote (equal to `effective`)
/// is replaced or removed; anything else found on disk is left alone.
fn roll_back(
    repo: &Repo,
    ws_lock: &WorkspaceLock,
    tmp: &Path,
    original: &FlatTree,
    effective: &FlatTree,
    head_files: &FlatTree,
    err: ConcurvError,
) -> ConcurvError {
    let result = (|| -> Result<()> {
        let now = scan(repo.root(), ScanMode::HashOnly, head_files)?;
        let mut back = FlatTree::new();
        for (p, st) in original {
            match now.get(p) {
                Some(n) if n != st && Some(n) != effective.get(p) => {
                    back.insert(p.clone(), *n); // a foreign change: keep it
                }
                _ => {
                    back.insert(p.clone(), *st);
                }
            }
        }
        for (p, n) in &now {
            if !original.contains_key(p) && Some(n) != effective.get(p) {
                back.insert(p.clone(), *n); // not written by us: keep it
            }
        }
        apply(tmp, repo, &now, &back, head_files)?;
        if scan(repo.root(), ScanMode::HashOnly, head_files)? != back {
            return Err(ConcurvError::new(
                ErrorCode::ConcurrentModification,
                "working copy changed during the rollback",
            ));
        }
        clear_marker(repo, ws_lock)
    })();
    match result {
        Ok(()) => ConcurvError::new(
            err.code,
            format!(
                "{}; the working copy was rolled back, nothing changed",
                err.message
            ),
        ),
        Err(e2) => ConcurvError::new(
            err.code,
            format!(
                "{}; rolling the working copy back also failed ({}). Every file holds either its \
                 previous or its new content; `concurv status` shows how to finish or undo the switch",
                err.message, e2.message
            ),
        ),
    }
}

/// The standard refusal for a dirty working copy.
pub(crate) fn dirty_error(dirty: &[crate::status::Change]) -> ConcurvError {
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
    ConcurvError::new(
        ErrorCode::DirtyWorkspace,
        format!(
            "working copy has {} unsaved change(s); nothing was modified.\n{}{more}\n\
             Save them first (`concurv save -m ...`) or pass --force to keep a backup snapshot and overwrite",
            dirty.len(),
            list.join("\n")
        ),
    )
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
    tmp: &Path,
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
        let current = current_state(root, path, head)?;
        if current == Some(*want) {
            // Already correct on disk (e.g. written by an interrupted restore).
            continue;
        }
        if current != have.copied() {
            return Err(changed_underneath(path));
        }
        materialize(repo, tmp, path, want, &native)?;
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

fn materialize(
    repo: &Repo,
    tmp: &Path,
    path: &RepoPath,
    want: &FileState,
    native: &Path,
) -> Result<()> {
    let store = repo.store();
    match want.kind {
        EntryKind::File | EntryKind::Executable => {
            let mut t = TempFile::create(tmp, "restore", store.faults())?;
            store.read_blob_to(&want.blob, &mut TempWriter(&mut t))?;
            set_mode(t.path(), want.kind == EntryKind::Executable)?;
            t.sync()?;
            t.persist(native)
        }
        EntryKind::Symlink => make_symlink(repo, tmp, path, want, native),
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
fn make_symlink(
    repo: &Repo,
    tmp_dir: &Path,
    _path: &RepoPath,
    want: &FileState,
    native: &Path,
) -> Result<()> {
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
    let tmp = tmp_dir.join(format!(
        "link.{}.tmp",
        concurv_core::RandomId::generate()?.to_hex()
    ));
    std::os::unix::fs::symlink(OsStr::from_bytes(&target), &tmp)
        .map_err(|e| ConcurvError::io(e, "create symlink", &tmp))?;
    rename_durable(&tmp, native, store.faults())
}

#[cfg(not(unix))]
fn make_symlink(
    _repo: &Repo,
    _tmp: &Path,
    path: &RepoPath,
    _want: &FileState,
    _native: &Path,
) -> Result<()> {
    let _ = rename_durable;
    Err(ConcurvError::new(
        ErrorCode::UnsupportedFileType,
        format!("cannot create symlink `{path}` on this platform"),
    ))
}

/// Paths on disk that writing `target` would overwrite although they are not
/// part of the scanned working copy (ignored, untracked): a file or symlink at
/// a target path, or at a directory the target needs. Directories in the way
/// are left to `apply`, which refuses them.
fn ignored_collisions(
    root: &Path,
    working: &FlatTree,
    target: &FlatTree,
    head: &FlatTree,
) -> Result<Vec<RepoPath>> {
    let mut out = std::collections::BTreeSet::new();
    for path in target.keys() {
        if working.contains_key(path) {
            continue;
        }
        let comps = path.components();
        for depth in 1..=comps.len() {
            let p = RepoPath::parse(&comps[..depth].join("/"))?;
            if working.contains_key(&p) {
                // A tracked file there is replaced or removed by the plan.
                break;
            }
            match stat_path(&p.to_native(root))? {
                Present::Missing => break,
                Present::Dir => {}
                Present::File { .. } | Present::Symlink(_) => {
                    // Already holding exactly the target content (e.g. an
                    // interrupted restore being resumed): nothing to lose.
                    if depth == comps.len()
                        && current_state(root, &p, head)? == target.get(&p).copied()
                    {
                        break;
                    }
                    out.insert(p);
                    break;
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}
