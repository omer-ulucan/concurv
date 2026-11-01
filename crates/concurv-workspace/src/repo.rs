//! Repository discovery, initialization and opening, for the primary
//! workspace and secondary workspaces that share its store (RFC 0004 §3).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use concurv_core::{
    ConcurvError, DeviceId, ErrorCode, OperationId, RandomId, Result, SnapshotId, WorkspaceId,
};
use concurv_objects::fs::{
    FileLock, TempSession, clean_tmp_root, create_dir_durable, rename_durable, sync_dir,
    write_file_atomic,
};
use concurv_objects::{Faults, ObjectStore, Operation, View, WorkspaceView};
use concurv_ops::OpLog;
use concurv_refs::{OpHead, RepoLock};

use crate::worktree::{FlatTree, flatten};

/// Contents of `.concurv/format` in the primary workspace (holds the store).
pub const REPO_FORMAT: &str = "concurv-repo-format 1\n";
/// Contents of `.concurv/format` in a secondary workspace.
pub const WORKSPACE_FORMAT: &str = "concurv-workspace-format 1\n";
/// Default branch created by `init`.
pub const DEFAULT_BRANCH: &str = "main";
/// Name of the workspace created by `init`.
pub const DEFAULT_WORKSPACE: &str = "default";
/// Workspace-local temp directory inside a workspace's `.concurv/`. Distinct
/// from the store's `tmp/` even in the primary workspace, where both live in
/// the same `.concurv/`: each temp root must be cleaned only under its own
/// lock (store `tmp/`: repository lock; `ws-tmp/`: workspace lock).
pub const WORKSPACE_TMP: &str = "ws-tmp";
/// Default bounded wait for the repository and workspace locks.
pub const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Options controlling a repository handle (mostly for tests).
#[derive(Clone, Debug, Default)]
pub struct RepoOptions {
    /// Fault injection for durable writes.
    pub faults: Faults,
    /// Fixed clock in ms since epoch (deterministic tests); `None` = system time.
    pub now_ms: Option<i64>,
    /// Author override; otherwise `CONCURV_AUTHOR`, then the OS user name.
    pub author: Option<String>,
    /// How long to wait for a busy lock; `None` = [`DEFAULT_LOCK_TIMEOUT`].
    pub lock_timeout: Option<Duration>,
}

/// An open workspace and the repository (store + op log) it belongs to.
#[derive(Debug)]
pub struct Repo {
    root: PathBuf,
    ws_meta: PathBuf,
    store_meta: PathBuf,
    store: ObjectStore,
    op_head: OpHead,
    oplog: OpLog,
    device_id: DeviceId,
    workspace_id: WorkspaceId,
    primary: bool,
    opts: RepoOptions,
}

/// Exclusive lock on one workspace's files (RFC 0004 §5).
#[derive(Debug)]
pub struct WorkspaceLock {
    _lock: FileLock,
}

/// A temp session in the store's `tmp/` plus a store handle writing there,
/// for lock-free object writes.
#[derive(Debug)]
pub struct StoreWriter {
    _session: TempSession,
    store: ObjectStore,
}

impl StoreWriter {
    /// Store handle whose temp files go to this writer's session.
    pub fn store(&self) -> &ObjectStore {
        &self.store
    }
}

fn read_id_file(path: &Path) -> Result<RandomId> {
    let s = fs::read_to_string(path).map_err(|e| ConcurvError::io(e, "read", path))?;
    RandomId::from_hex(s.trim_end_matches('\n')).map_err(|_| {
        ConcurvError::new(ErrorCode::ObjectCorrupt, "malformed id file in .concurv").with_path(path)
    })
}

fn read_format(meta: &Path) -> Result<String> {
    let p = meta.join("format");
    fs::read_to_string(&p).map_err(|e| ConcurvError::io(e, "read", &p))
}

/// Ensure `<root>/.concurv` is a real directory (not a symlink/junction).
pub(crate) fn check_meta_dir(root: &Path) -> Result<Option<PathBuf>> {
    let meta_dir = root.join(".concurv");
    match fs::symlink_metadata(&meta_dir) {
        Ok(m) if m.file_type().is_symlink() => Err(ConcurvError::new(
            ErrorCode::InvalidPath,
            format!(
                "`{}` is a symlink; refusing to use it as repository metadata",
                meta_dir.display()
            ),
        )
        .with_path(&meta_dir)),
        Ok(m) if m.is_dir() => Ok(Some(meta_dir)),
        Ok(_) => Err(ConcurvError::new(
            ErrorCode::InvalidPath,
            format!("`{}` exists but is not a directory", meta_dir.display()),
        )
        .with_path(&meta_dir)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConcurvError::io(e, "inspect", &meta_dir)),
    }
}

fn unsupported(format: &str, p: &Path) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::UnsupportedFormat,
        format!(
            "repository format {:?} is not supported by this concurv build",
            format.trim()
        ),
    )
    .with_path(p.join("format"))
}

impl Repo {
    /// Find the workspace containing `start` (searching parent directories).
    pub fn discover(start: &Path, opts: RepoOptions) -> Result<Self> {
        let start = fs::canonicalize(start)
            .map(simplify)
            .map_err(|e| ConcurvError::io(e, "resolve", start))?;
        let mut cur: Option<&Path> = Some(&start);
        while let Some(dir) = cur {
            if check_meta_dir(dir)?.is_some() {
                return Self::open(dir, opts);
            }
            cur = dir.parent();
        }
        Err(ConcurvError::new(
            ErrorCode::NotARepo,
            format!(
                "`{}` is not inside a concurv repository (run `concurv init`)",
                start.display()
            ),
        ))
    }

    /// Open the workspace whose root is exactly `root` (primary or secondary).
    pub fn open(root: &Path, opts: RepoOptions) -> Result<Self> {
        let Some(ws_meta) = check_meta_dir(root)? else {
            return Err(ConcurvError::new(
                ErrorCode::NotARepo,
                format!("no repository at `{}`", root.display()),
            ));
        };
        let format = read_format(&ws_meta)?;
        let (store_meta, primary) = if format == REPO_FORMAT {
            (ws_meta.clone(), true)
        } else if format == WORKSPACE_FORMAT {
            let p = ws_meta.join("store");
            let s = fs::read_to_string(&p).map_err(|e| ConcurvError::io(e, "read", &p))?;
            let store_meta = PathBuf::from(s.trim_end_matches('\n'));
            let real = fs::symlink_metadata(&store_meta).is_ok_and(|m| m.is_dir());
            if !real || read_format(&store_meta).ok().as_deref() != Some(REPO_FORMAT) {
                return Err(ConcurvError::new(
                    ErrorCode::NotARepo,
                    format!(
                        "this workspace belongs to the repository at `{}`, which is missing or not a concurv repository",
                        store_meta.display()
                    ),
                )
                .with_path(&p));
            }
            (store_meta, false)
        } else {
            return Err(unsupported(&format, &ws_meta));
        };
        let device_id = DeviceId::from_random(read_id_file(&store_meta.join("device_id"))?);
        let workspace_id = WorkspaceId::from_random(read_id_file(&ws_meta.join("workspace_id"))?);
        let store = ObjectStore::new(&store_meta, opts.faults.clone());
        let op_head = OpHead::new(&store_meta, opts.faults.clone());
        let oplog = OpLog::new(store.clone(), op_head.clone());
        Ok(Self {
            root: root.to_path_buf(),
            ws_meta,
            store_meta,
            store,
            op_head,
            oplog,
            device_id,
            workspace_id,
            primary,
            opts,
        })
    }

    /// Create a new repository at `root` atomically (RFC 0002 §3.3). `root`
    /// becomes the primary workspace, named [`DEFAULT_WORKSPACE`].
    pub fn init(root: &Path, opts: RepoOptions) -> Result<Self> {
        let meta = fs::metadata(root).map_err(|e| ConcurvError::io(e, "inspect", root))?;
        if !meta.is_dir() {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!("`{}` is not a directory", root.display()),
            ));
        }
        if check_meta_dir(root)?.is_some() || root.join(".concurv").exists() {
            return Err(ConcurvError::new(
                ErrorCode::AlreadyInitialized,
                format!("`{}` already contains a concurv repository", root.display()),
            ));
        }
        let faults = &opts.faults;
        let staging = root.join(format!(".concurv-init-{}", RandomId::generate()?.to_hex()));
        create_dir_durable(&staging, faults)?;
        concurv_objects::store::create_layout(&staging, faults)?;
        let tmp = staging.join("tmp");
        let device_id = DeviceId::generate()?;
        let workspace_id = WorkspaceId::generate()?;
        write_file_atomic(
            &tmp,
            &staging.join("format"),
            REPO_FORMAT.as_bytes(),
            faults,
        )?;
        write_file_atomic(
            &tmp,
            &staging.join("device_id"),
            format!("{}\n", device_id.to_hex()).as_bytes(),
            faults,
        )?;
        write_file_atomic(
            &tmp,
            &staging.join("workspace_id"),
            format!("{}\n", workspace_id.to_hex()).as_bytes(),
            faults,
        )?;
        let store = ObjectStore::new(&staging, faults.clone());
        let mut workspaces = BTreeMap::new();
        workspaces.insert(
            workspace_id,
            WorkspaceView {
                name: DEFAULT_WORKSPACE.to_owned(),
                branch: Some(DEFAULT_BRANCH.to_owned()),
                head: None,
                merge: None,
            },
        );
        let root_op = Operation {
            parents: Vec::new(),
            timestamp_ms: now_ms(&opts),
            device_id,
            workspace_id: None,
            kind: "init".into(),
            description: "initialize repository".into(),
            backup: None,
            undoes: None,
            checked_out: None,
            view: View {
                branches: BTreeMap::new(),
                workspaces,
            },
        };
        let op_id = store.write_operation(&root_op)?;
        OpHead::new(&staging, faults.clone()).write_initial(&op_id)?;
        sync_dir(&staging, faults)?;
        rename_durable(&staging, &root.join(".concurv"), faults)?;
        Self::open(root, opts)
    }

    // -------------------------------------------------------------- accessors

    /// Workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// The store's `.concurv` directory (in the primary workspace).
    pub fn store_dir(&self) -> &Path {
        &self.store_meta
    }
    /// This workspace's `.concurv` directory (equals [`Self::store_dir`] for
    /// the primary workspace).
    pub fn workspace_meta_dir(&self) -> &Path {
        &self.ws_meta
    }
    /// True for the workspace created by `init`.
    pub fn is_primary(&self) -> bool {
        self.primary
    }
    /// Object store (for reads; writers use [`Self::store_writer`]).
    pub fn store(&self) -> &ObjectStore {
        &self.store
    }
    /// Operation log.
    pub fn oplog(&self) -> &OpLog {
        &self.oplog
    }
    /// Op-head pointer.
    pub fn op_head(&self) -> &OpHead {
        &self.op_head
    }
    /// This clone's device id.
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }
    /// The workspace at `root`.
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }
    /// Options.
    pub fn options(&self) -> &RepoOptions {
        &self.opts
    }
    fn lock_timeout(&self) -> Duration {
        self.opts.lock_timeout.unwrap_or(DEFAULT_LOCK_TIMEOUT)
    }

    // ------------------------------------------------------------------ locks

    /// Take the repository lock (bounded wait) and remove debris of
    /// interrupted commands from the store's `tmp/`. Hold it only for the
    /// commit step.
    pub fn lock(&self) -> Result<RepoLock> {
        let lock = RepoLock::acquire_timeout(&self.store_meta, self.lock_timeout())?;
        concurv_ops::clean_tmp(&lock, self.store.tmp_dir())?;
        Ok(lock)
    }

    /// Take this workspace's lock (bounded wait) and remove debris from the
    /// workspace's own `ws-tmp/` (never the store's `tmp/`).
    pub fn workspace_lock(&self) -> Result<WorkspaceLock> {
        let lock = FileLock::acquire(
            &self.ws_meta.join("ws.lock"),
            self.lock_timeout(),
            "this workspace's lock",
        )?;
        let tmp = self.ws_meta.join(WORKSPACE_TMP);
        create_dir_durable(&tmp, &self.opts.faults)?;
        clean_tmp_root(&tmp)?;
        Ok(WorkspaceLock { _lock: lock })
    }

    /// Start a temp session in the store for lock-free object writes. Briefly
    /// takes the repository lock, which serializes session creation with
    /// recovery (RFC 0004 §5).
    pub fn store_writer(&self) -> Result<StoreWriter> {
        let session = {
            let _lock = self.lock()?;
            TempSession::create(self.store.tmp_dir(), &self.opts.faults)?
        };
        let store = self.store.with_tmp(session.dir());
        Ok(StoreWriter {
            _session: session,
            store,
        })
    }

    /// Start a temp session in this workspace's `ws-tmp/` (same filesystem as
    /// the working files) for materializing files. Requires the workspace lock.
    pub(crate) fn workspace_session(&self, _lock: &WorkspaceLock) -> Result<TempSession> {
        TempSession::create(&self.ws_meta.join(WORKSPACE_TMP), &self.opts.faults)
    }

    // ------------------------------------------------------------------ misc

    /// Current time per options.
    pub fn now_ms(&self) -> i64 {
        now_ms(&self.opts)
    }

    /// Author string per options / environment.
    pub fn author(&self) -> String {
        if let Some(a) = &self.opts.author {
            return a.clone();
        }
        for var in ["CONCURV_AUTHOR", "USER", "USERNAME"] {
            if let Ok(v) = std::env::var(var)
                && !v.trim().is_empty()
            {
                return v.trim().to_owned();
            }
        }
        "unknown".into()
    }

    /// Current op head and this workspace's view entry.
    pub fn current(&self) -> Result<(OperationId, Operation, WorkspaceView)> {
        let (id, op) = self.oplog.head()?;
        let ws = op
            .view
            .workspaces
            .get(&self.workspace_id)
            .cloned()
            .ok_or_else(|| {
                ConcurvError::new(
                    ErrorCode::NotFound,
                    "this workspace is not registered in the repository (was it created by a different repository?)",
                )
            })?;
        Ok((id, op, ws))
    }

    /// The workspace head snapshot, if any.
    pub fn head_snapshot(&self) -> Result<Option<SnapshotId>> {
        Ok(self.current()?.2.head)
    }

    /// Flat file map of a snapshot (trees verified; blobs not).
    pub fn snapshot_files(&self, id: &SnapshotId) -> Result<FlatTree> {
        let snap = self.store.read_snapshot(id)?;
        flatten(&self.store, &snap.tree, None)
    }

    /// Flat file map of the workspace head (empty before the first save).
    pub fn head_files(&self, head: Option<&SnapshotId>) -> Result<FlatTree> {
        match head {
            Some(h) => self.snapshot_files(h),
            None => Ok(FlatTree::new()),
        }
    }

    /// Resolve user input (full id or ≥ 8-char prefix) to a snapshot id.
    pub fn resolve_snapshot(&self, input: &str) -> Result<SnapshotId> {
        let id = self.store.resolve_prefix(input)?;
        let sid = SnapshotId::from_object_id(id);
        self.store.read_snapshot(&sid).map_err(|e| {
            if e.code == ErrorCode::WrongObjectKind {
                ConcurvError::new(ErrorCode::NotFound, format!("{id} is not a snapshot"))
            } else {
                e
            }
        })?;
        Ok(sid)
    }

    /// Path of this workspace's interrupted-restore marker.
    pub fn restore_marker(&self) -> PathBuf {
        self.ws_meta.join("restore-in-progress")
    }

    /// Locator file recording a secondary workspace's root path.
    pub(crate) fn locator_path(&self, ws: &WorkspaceId) -> PathBuf {
        self.store_meta.join("workspaces").join(ws.to_hex())
    }
}

fn now_ms(opts: &RepoOptions) -> i64 {
    opts.now_ms.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    })
}

/// On Windows, `canonicalize` returns verbatim paths (`\\?\C:\...`). Strip
/// the prefix for ordinary drive-letter paths so stored and printed paths
/// look normal; UNC and other verbatim forms are kept as they are.
pub(crate) fn simplify(p: PathBuf) -> PathBuf {
    if cfg!(windows)
        && let Some(s) = p.to_str()
        && let Some(rest) = s.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
        && rest.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
    {
        return PathBuf::from(rest);
    }
    p
}
