//! Repository discovery, initialization and opening.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use concurv_core::{
    ConcurvError, DeviceId, ErrorCode, OperationId, RandomId, Result, SnapshotId, WorkspaceId,
};
use concurv_objects::fs::{create_dir_durable, rename_durable, sync_dir, write_file_atomic};
use concurv_objects::{Faults, ObjectStore, Operation, View, WorkspaceView};
use concurv_ops::OpLog;
use concurv_refs::{OpHead, RepoLock};

use crate::worktree::{FlatTree, flatten};

/// Contents of `.concurv/format`.
pub const REPO_FORMAT: &str = "concurv-repo-format 1\n";
/// Default branch created by `init`.
pub const DEFAULT_BRANCH: &str = "main";

/// Options controlling a repository handle (mostly for tests).
#[derive(Clone, Debug, Default)]
pub struct RepoOptions {
    /// Fault injection for durable writes.
    pub faults: Faults,
    /// Fixed clock in ms since epoch (deterministic tests); `None` = system time.
    pub now_ms: Option<i64>,
    /// Author override; otherwise `CONCURV_AUTHOR`, then the OS user name.
    pub author: Option<String>,
}

/// An open repository with its single (R1) workspace at `root`.
#[derive(Debug)]
pub struct Repo {
    root: PathBuf,
    meta_dir: PathBuf,
    store: ObjectStore,
    op_head: OpHead,
    oplog: OpLog,
    device_id: DeviceId,
    workspace_id: WorkspaceId,
    opts: RepoOptions,
}

fn read_id_file(path: &Path) -> Result<RandomId> {
    let s = fs::read_to_string(path).map_err(|e| ConcurvError::io(e, "read", path))?;
    RandomId::from_hex(s.trim_end_matches('\n')).map_err(|_| {
        ConcurvError::new(ErrorCode::ObjectCorrupt, "malformed id file in .concurv").with_path(path)
    })
}

/// Ensure `.concurv` under `root` is a real directory (not a symlink/junction).
fn check_meta_dir(root: &Path) -> Result<Option<PathBuf>> {
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

impl Repo {
    /// Find the repository containing `start` (searching parent directories).
    pub fn discover(start: &Path, opts: RepoOptions) -> Result<Self> {
        let start = fs::canonicalize(start).map_err(|e| ConcurvError::io(e, "resolve", start))?;
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
                "`{}` is not inside an concurv repository (run `concurv init`)",
                start.display()
            ),
        ))
    }

    /// Open the repository whose workspace root is exactly `root`.
    pub fn open(root: &Path, opts: RepoOptions) -> Result<Self> {
        let Some(meta_dir) = check_meta_dir(root)? else {
            return Err(ConcurvError::new(
                ErrorCode::NotARepo,
                format!("no repository at `{}`", root.display()),
            ));
        };
        let fmt_path = meta_dir.join("format");
        let format =
            fs::read_to_string(&fmt_path).map_err(|e| ConcurvError::io(e, "read", &fmt_path))?;
        if format != REPO_FORMAT {
            return Err(ConcurvError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "repository format {:?} is not supported by this concurv build",
                    format.trim()
                ),
            )
            .with_path(&fmt_path));
        }
        let device_id = DeviceId::from_random(read_id_file(&meta_dir.join("device_id"))?);
        let workspace_id = WorkspaceId::from_random(read_id_file(&meta_dir.join("workspace_id"))?);
        let store = ObjectStore::new(&meta_dir, opts.faults.clone());
        let op_head = OpHead::new(&meta_dir, opts.faults.clone());
        let oplog = OpLog::new(store.clone(), op_head.clone());
        Ok(Self {
            root: root.to_path_buf(),
            meta_dir,
            store,
            op_head,
            oplog,
            device_id,
            workspace_id,
            opts,
        })
    }

    /// Create a new repository at `root` atomically (RFC 0002 §3.3).
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
                format!(
                    "`{}` already contains an concurv repository",
                    root.display()
                ),
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
                branch: Some(DEFAULT_BRANCH.to_owned()),
                head: None,
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
    /// `.concurv` directory.
    pub fn meta_dir(&self) -> &Path {
        &self.meta_dir
    }
    /// Object store.
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

    /// Take the writer lock and remove debris from interrupted transactions.
    pub fn lock(&self) -> Result<RepoLock> {
        let lock = RepoLock::acquire(&self.meta_dir)?;
        concurv_ops::clean_tmp(&lock, self.store.tmp_dir())?;
        Ok(lock)
    }

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
                    ErrorCode::ObjectCorrupt,
                    "current operation has no entry for this workspace",
                )
            })?;
        Ok((id, op, ws))
    }

    /// The workspace head snapshot, if any save happened.
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

    /// Path of the interrupted-restore marker.
    pub fn restore_marker(&self) -> PathBuf {
        self.meta_dir.join("restore-in-progress")
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
