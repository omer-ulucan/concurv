//! Canonical object encoding (RFC 0001 §3). Hand-written so that the exact
//! hashed bytes are specified independently of any serializer.

use std::collections::BTreeMap;

use concurv_core::path::MAX_NAME_LEN;
use concurv_core::{
    BlobId, ChangeId, ConcurvError, DeviceId, ErrorCode, OBJECT_ID_LEN, ObjectId, OperationId,
    RANDOM_ID_LEN, RandomId, Result, SnapshotId, TreeId, WorkspaceId, validate_name,
};

/// Object magic for format version 1.
pub const MAGIC: [u8; 4] = *b"CNV1";
/// Header length: magic(4) + kind(1) + payload_len(8).
pub const HEADER_LEN: usize = 13;
/// BLAKE3 derive-key context for object ids.
pub const HASH_CONTEXT: &str = "concurv 2026-11 object v1";
/// Maximum payload of a non-blob object.
pub const MAX_STRUCTURED_PAYLOAD: u64 = 64 << 20;
/// Maximum entries in one tree.
pub const MAX_TREE_ENTRIES: u32 = 1_000_000;
/// Maximum parents of a snapshot or operation.
pub const MAX_PARENTS: u32 = 1024;
/// Maximum length of a string field.
pub const MAX_STRING: u32 = 16 << 20;

/// Object kind byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ObjectKind {
    /// File content or symlink target.
    Blob = 1,
    /// Directory listing.
    Tree = 2,
    /// Code state with history.
    Snapshot = 3,
    /// Repository mutation record.
    Operation = 4,
}

impl ObjectKind {
    /// Parse a kind byte.
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            1 => Ok(Self::Blob),
            2 => Ok(Self::Tree),
            3 => Ok(Self::Snapshot),
            4 => Ok(Self::Operation),
            other => Err(ConcurvError::new(
                ErrorCode::UnsupportedFormat,
                format!("unknown object kind {other}"),
            )),
        }
    }

    /// Lowercase name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Snapshot => "snapshot",
            Self::Operation => "operation",
        }
    }
}

/// Encode an object header.
pub fn encode_header(kind: ObjectKind, payload_len: u64) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[..4].copy_from_slice(&MAGIC);
    h[4] = kind as u8;
    h[5..].copy_from_slice(&payload_len.to_be_bytes());
    h
}

/// Decode and validate an object header.
pub fn decode_header(h: &[u8]) -> Result<(ObjectKind, u64)> {
    if h.len() < HEADER_LEN {
        return Err(ConcurvError::new(
            ErrorCode::ObjectCorrupt,
            "object shorter than its header",
        ));
    }
    if h[..4] != MAGIC {
        if &h[..3] == b"CNV" {
            return Err(ConcurvError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "object format version {:?} is not supported by this concurv build",
                    h[3] as char
                ),
            ));
        }
        return Err(ConcurvError::new(
            ErrorCode::ObjectCorrupt,
            "bad object magic",
        ));
    }
    let kind = ObjectKind::from_byte(h[4])?;
    let mut len = [0u8; 8];
    len.copy_from_slice(&h[5..HEADER_LEN]);
    let len = u64::from_be_bytes(len);
    if kind != ObjectKind::Blob && len > MAX_STRUCTURED_PAYLOAD {
        return Err(ConcurvError::new(
            ErrorCode::LimitExceeded,
            format!("{} payload of {len} bytes exceeds limit", kind.name()),
        ));
    }
    Ok((kind, len))
}

/// Incremental object hasher (feed header and payload bytes in order).
pub struct ObjectHasher(blake3::Hasher);

impl ObjectHasher {
    /// New hasher in derive-key mode.
    pub fn new() -> Self {
        Self(blake3::Hasher::new_derive_key(HASH_CONTEXT))
    }
    /// Feed bytes.
    pub fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    /// Final id.
    pub fn finalize(&self) -> ObjectId {
        ObjectId::from_bytes(*self.0.finalize().as_bytes())
    }
}

impl Default for ObjectHasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Hash a complete object given its payload.
pub fn hash_object(kind: ObjectKind, payload: &[u8]) -> ObjectId {
    let mut h = ObjectHasher::new();
    h.update(&encode_header(kind, payload.len() as u64));
    h.update(payload);
    h.finalize()
}

/// Full canonical bytes (header + payload).
pub fn encode_object(kind: ObjectKind, payload: &[u8]) -> Result<Vec<u8>> {
    if kind != ObjectKind::Blob && payload.len() as u64 > MAX_STRUCTURED_PAYLOAD {
        return Err(ConcurvError::new(
            ErrorCode::LimitExceeded,
            format!("{} payload exceeds limit", kind.name()),
        ));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.extend_from_slice(&encode_header(kind, payload.len() as u64));
    out.extend_from_slice(payload);
    Ok(out)
}

/// Split full object bytes into payload after validating header, length and kind.
pub fn decode_object(bytes: &[u8], expected: ObjectKind) -> Result<&[u8]> {
    let (kind, len) = decode_header(bytes)?;
    if kind != expected {
        return Err(ConcurvError::new(
            ErrorCode::WrongObjectKind,
            format!("expected {}, found {}", expected.name(), kind.name()),
        ));
    }
    let payload = &bytes[HEADER_LEN..];
    if payload.len() as u64 != len {
        return Err(ConcurvError::new(
            ErrorCode::ObjectCorrupt,
            format!(
                "header declares {len} payload bytes but object has {}",
                payload.len()
            ),
        ));
    }
    Ok(payload)
}

// ---------------------------------------------------------------------------
// Primitive writer / reader

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn id(&mut self, id: &ObjectId) {
        self.0.extend_from_slice(id.as_bytes());
    }
    fn rid(&mut self, id: &RandomId) {
        self.0.extend_from_slice(id.as_bytes());
    }
    fn count(&mut self, n: usize, max: u32, what: &str) -> Result<()> {
        let n = u32::try_from(n).ok().filter(|n| *n <= max).ok_or_else(|| {
            ConcurvError::new(ErrorCode::LimitExceeded, format!("too many {what}"))
        })?;
        self.u32(n);
        Ok(())
    }
    fn string(&mut self, s: &str) -> Result<()> {
        self.count(s.len(), MAX_STRING, "bytes in string field")?;
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn corrupt(msg: impl Into<String>) -> ConcurvError {
    ConcurvError::new(ErrorCode::ObjectCorrupt, msg)
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| corrupt("unexpected end of object payload"))?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i64(&mut self) -> Result<i64> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.take(8)?);
        Ok(i64::from_be_bytes(a))
    }
    fn id(&mut self) -> Result<ObjectId> {
        let mut a = [0u8; OBJECT_ID_LEN];
        a.copy_from_slice(self.take(OBJECT_ID_LEN)?);
        Ok(ObjectId::from_bytes(a))
    }
    fn rid(&mut self) -> Result<RandomId> {
        let mut a = [0u8; RANDOM_ID_LEN];
        a.copy_from_slice(self.take(RANDOM_ID_LEN)?);
        Ok(RandomId::from_bytes(a))
    }
    fn count(&mut self, max: u32, what: &str) -> Result<u32> {
        let n = self.u32()?;
        if n > max {
            return Err(ConcurvError::new(
                ErrorCode::LimitExceeded,
                format!("{n} {what} exceeds limit {max}"),
            ));
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String> {
        let n = self.count(MAX_STRING, "string bytes")?;
        let b = self.take(n as usize)?;
        String::from_utf8(b.to_vec()).map_err(|_| corrupt("string field is not valid UTF-8"))
    }
    fn flag(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(corrupt(format!("invalid boolean byte {other}"))),
        }
    }
    fn finish(self) -> Result<()> {
        if self.pos != self.buf.len() {
            return Err(corrupt("trailing bytes after object payload"));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tree

/// Kind of a tree entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EntryKind {
    /// Regular file.
    File = 1,
    /// Regular file with the executable bit.
    Executable = 2,
    /// Symbolic link; blob holds the raw target.
    Symlink = 3,
    /// Subdirectory.
    Tree = 4,
}

impl EntryKind {
    fn from_byte(b: u8) -> Result<Self> {
        match b {
            1 => Ok(Self::File),
            2 => Ok(Self::Executable),
            3 => Ok(Self::Symlink),
            4 => Ok(Self::Tree),
            other => Err(corrupt(format!("unknown tree entry kind {other}"))),
        }
    }
    /// True for kinds whose target is a blob.
    pub fn is_blob(self) -> bool {
        self != Self::Tree
    }
}

/// One tree entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Validated name.
    pub name: String,
    /// Entry kind.
    pub kind: EntryKind,
    /// Target id (blob for files/symlinks, tree for subdirectories).
    pub id: ObjectId,
}

/// A directory listing, always canonically sorted.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Tree {
    entries: Vec<TreeEntry>,
}

impl Tree {
    /// Build a tree: validates names, sorts by name bytes, rejects duplicates.
    pub fn new(mut entries: Vec<TreeEntry>) -> Result<Self> {
        for e in &entries {
            validate_name(&e.name)?;
        }
        if entries.len() > MAX_TREE_ENTRIES as usize {
            return Err(ConcurvError::new(
                ErrorCode::LimitExceeded,
                "too many tree entries",
            ));
        }
        entries.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
        if let Some(w) = entries.windows(2).find(|w| w[0].name == w[1].name) {
            return Err(ConcurvError::new(
                ErrorCode::InvalidPath,
                format!("duplicate entry name {:?}", w[0].name),
            ));
        }
        Ok(Self { entries })
    }

    /// Entries in canonical order.
    pub fn entries(&self) -> &[TreeEntry] {
        &self.entries
    }

    /// Look up an entry by name.
    pub fn get(&self, name: &str) -> Option<&TreeEntry> {
        self.entries
            .binary_search_by(|e| e.name.as_bytes().cmp(name.as_bytes()))
            .ok()
            .map(|i| &self.entries[i])
    }

    /// True if there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Canonical payload bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = Writer(Vec::with_capacity(4 + self.entries.len() * 48));
        w.count(self.entries.len(), MAX_TREE_ENTRIES, "tree entries")?;
        for e in &self.entries {
            w.u8(e.kind as u8);
            // validate_name guarantees len <= 255.
            w.u16(e.name.len() as u16);
            w.0.extend_from_slice(e.name.as_bytes());
            w.id(&e.id);
        }
        Ok(w.0)
    }

    /// Strictly decode a payload; rejects any non-canonical encoding.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        let n = r.count(MAX_TREE_ENTRIES, "tree entries")?;
        let mut entries: Vec<TreeEntry> = Vec::with_capacity((n as usize).min(4096));
        for _ in 0..n {
            let kind = EntryKind::from_byte(r.u8()?)?;
            let len = r.u16()? as usize;
            if len > MAX_NAME_LEN {
                return Err(corrupt("tree entry name too long"));
            }
            let name = String::from_utf8(r.take(len)?.to_vec())
                .map_err(|_| corrupt("tree entry name is not UTF-8"))?;
            validate_name(&name).map_err(|e| corrupt(e.message))?;
            if let Some(prev) = entries.last()
                && prev.name.as_bytes() >= name.as_bytes()
            {
                return Err(corrupt("tree entries are not strictly sorted"));
            }
            let id = r.id()?;
            entries.push(TreeEntry { name, kind, id });
        }
        r.finish()?;
        Ok(Self { entries })
    }
}

// ---------------------------------------------------------------------------
// Snapshot

/// An immutable code state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Root tree.
    pub tree: TreeId,
    /// Ordered parents (first = mainline).
    pub parents: Vec<SnapshotId>,
    /// Logical change id.
    pub change_id: ChangeId,
    /// Creation time, ms since Unix epoch (UTC).
    pub timestamp_ms: i64,
    /// Author string.
    pub author: String,
    /// Message.
    pub message: String,
}

fn check_unique<T: Ord + Copy>(items: &[T], what: &str) -> Result<()> {
    let mut v = items.to_vec();
    v.sort();
    if v.windows(2).any(|w| w[0] == w[1]) {
        return Err(corrupt(format!("duplicate {what}")));
    }
    Ok(())
}

impl Snapshot {
    /// Canonical payload bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        check_unique(&self.parents, "snapshot parent")?;
        let mut w = Writer(Vec::new());
        w.id(&self.tree.object_id());
        w.count(self.parents.len(), MAX_PARENTS, "parents")?;
        for p in &self.parents {
            w.id(&p.object_id());
        }
        w.rid(&self.change_id.random_id());
        w.i64(self.timestamp_ms);
        w.string(&self.author)?;
        w.string(&self.message)?;
        Ok(w.0)
    }

    /// Strictly decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        let tree = TreeId::from_object_id(r.id()?);
        let n = r.count(MAX_PARENTS, "parents")?;
        let mut parents = Vec::with_capacity(n as usize);
        for _ in 0..n {
            parents.push(SnapshotId::from_object_id(r.id()?));
        }
        check_unique(&parents, "snapshot parent")?;
        let change_id = ChangeId::from_random(r.rid()?);
        let timestamp_ms = r.i64()?;
        let author = r.string()?;
        let message = r.string()?;
        r.finish()?;
        Ok(Self {
            tree,
            parents,
            change_id,
            timestamp_ms,
            author,
            message,
        })
    }
}

// ---------------------------------------------------------------------------
// Operation and view

/// A workspace's pointers within a view.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct WorkspaceView {
    /// Unique human name (RFC 0004 §4), validated by [`validate_workspace_name`].
    pub name: String,
    /// Branch this workspace publishes to.
    pub branch: Option<String>,
    /// Snapshot the working copy is based on.
    pub head: Option<SnapshotId>,
}

/// Complete mutable repository state as of one operation.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct View {
    /// Branch name → head snapshot.
    pub branches: BTreeMap<String, SnapshotId>,
    /// Workspace → pointers.
    pub workspaces: BTreeMap<WorkspaceId, WorkspaceView>,
}

/// Validate a workspace name: 1–64 characters of `[A-Za-z0-9._-]`, not
/// starting with `.` or `-`.
pub fn validate_workspace_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !ok {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!(
                "invalid workspace name {name:?}: use 1-64 characters of A-Z a-z 0-9 . _ -, not starting with . or -"
            ),
        ));
    }
    Ok(())
}

/// Validate a branch name.
pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || name.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!("invalid branch name {name:?}"),
        ));
    }
    Ok(())
}

/// An append-only record of one repository mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation {
    /// Parent operations.
    pub parents: Vec<OperationId>,
    /// Time, ms since Unix epoch.
    pub timestamp_ms: i64,
    /// Device that performed it.
    pub device_id: DeviceId,
    /// Acting workspace, `None` for repository-level operations.
    pub workspace_id: Option<WorkspaceId>,
    /// Kind (`init`, `save`, `restore`).
    pub kind: String,
    /// Human description.
    pub description: String,
    /// Backup snapshot of discarded working-copy state, if any.
    pub backup: Option<SnapshotId>,
    /// Resulting view.
    pub view: View,
}

impl Operation {
    /// Canonical payload bytes.
    pub fn encode(&self) -> Result<Vec<u8>> {
        check_unique(&self.parents, "operation parent")?;
        let mut w = Writer(Vec::new());
        w.count(self.parents.len(), MAX_PARENTS, "parents")?;
        for p in &self.parents {
            w.id(&p.object_id());
        }
        w.i64(self.timestamp_ms);
        w.rid(&self.device_id.random_id());
        match &self.workspace_id {
            Some(ws) => {
                if ws.random_id().as_bytes() == &[0u8; RANDOM_ID_LEN] {
                    return Err(ConcurvError::internal("all-zero workspace id is reserved"));
                }
                w.rid(&ws.random_id())
            }
            None => w.rid(&RandomId::from_bytes([0; RANDOM_ID_LEN])),
        }
        w.string(&self.kind)?;
        w.string(&self.description)?;
        match &self.backup {
            Some(b) => {
                w.u8(1);
                w.id(&b.object_id());
            }
            None => w.u8(0),
        }
        w.count(self.view.branches.len(), MAX_TREE_ENTRIES, "branches")?;
        for (name, head) in &self.view.branches {
            validate_branch_name(name)?;
            w.string(name)?;
            w.id(&head.object_id());
        }
        w.count(self.view.workspaces.len(), MAX_TREE_ENTRIES, "workspaces")?;
        check_unique_names(self.view.workspaces.values().map(|w| w.name.as_str()))?;
        for (id, ws) in &self.view.workspaces {
            w.rid(&id.random_id());
            validate_workspace_name(&ws.name)?;
            w.string(&ws.name)?;
            match &ws.branch {
                Some(b) => {
                    w.u8(1);
                    w.string(b)?;
                }
                None => w.u8(0),
            }
            match &ws.head {
                Some(h) => {
                    w.u8(1);
                    w.id(&h.object_id());
                }
                None => w.u8(0),
            }
        }
        Ok(w.0)
    }

    /// Strictly decode a payload.
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut r = Reader::new(payload);
        let n = r.count(MAX_PARENTS, "parents")?;
        let mut parents = Vec::with_capacity(n as usize);
        for _ in 0..n {
            parents.push(OperationId::from_object_id(r.id()?));
        }
        check_unique(&parents, "operation parent")?;
        let timestamp_ms = r.i64()?;
        let device_id = DeviceId::from_random(r.rid()?);
        let ws = r.rid()?;
        let workspace_id =
            (ws.as_bytes() != &[0u8; RANDOM_ID_LEN]).then(|| WorkspaceId::from_random(ws));
        let kind = r.string()?;
        let description = r.string()?;
        let backup = if r.flag()? {
            Some(SnapshotId::from_object_id(r.id()?))
        } else {
            None
        };
        let mut view = View::default();
        let nb = r.count(MAX_TREE_ENTRIES, "branches")?;
        let mut last: Option<String> = None;
        for _ in 0..nb {
            let name = r.string()?;
            validate_branch_name(&name).map_err(|e| corrupt(e.message))?;
            if last
                .as_ref()
                .is_some_and(|l| l.as_bytes() >= name.as_bytes())
            {
                return Err(corrupt("branches are not strictly sorted"));
            }
            let head = SnapshotId::from_object_id(r.id()?);
            last = Some(name.clone());
            view.branches.insert(name, head);
        }
        let nw = r.count(MAX_TREE_ENTRIES, "workspaces")?;
        let mut last_ws: Option<WorkspaceId> = None;
        for _ in 0..nw {
            let id = WorkspaceId::from_random(r.rid()?);
            if last_ws.is_some_and(|l| l >= id) {
                return Err(corrupt("workspaces are not strictly sorted"));
            }
            last_ws = Some(id);
            let name = r.string()?;
            validate_workspace_name(&name).map_err(|e| corrupt(e.message))?;
            let branch = if r.flag()? { Some(r.string()?) } else { None };
            let head = if r.flag()? {
                Some(SnapshotId::from_object_id(r.id()?))
            } else {
                None
            };
            view.workspaces
                .insert(id, WorkspaceView { name, branch, head });
        }
        check_unique_names(view.workspaces.values().map(|w| w.name.as_str()))
            .map_err(|e| corrupt(e.message))?;
        r.finish()?;
        Ok(Self {
            parents,
            timestamp_ms,
            device_id,
            workspace_id,
            kind,
            description,
            backup,
            view,
        })
    }
}

/// Convenience: typed id of a blob payload.
pub fn blob_id(content: &[u8]) -> BlobId {
    BlobId::from_object_id(hash_object(ObjectKind::Blob, content))
}

fn check_unique_names<'a>(names: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for n in names {
        if !seen.insert(n) {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!("workspace name {n:?} is used by more than one workspace"),
            ));
        }
    }
    Ok(())
}
