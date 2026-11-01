//! Working-copy scanning and conversion between on-disk files, flat path
//! maps and stored trees.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::Path;

use concurv_core::path::{METADATA_DIR, PlatformRules};
use concurv_core::{BlobId, ConcurvError, ErrorCode, RepoPath, Result, TreeId};
use concurv_objects::encoding::blob_id;
use concurv_objects::{EntryKind, ObjectStore, Tree, TreeEntry, hash_blob_stream};

/// Maximum directory nesting accepted from a stored tree.
pub const MAX_TREE_DEPTH: usize = 512;

/// Versioned state of one non-directory path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileState {
    /// File, executable or symlink.
    pub kind: EntryKind,
    /// Content (or symlink target) blob.
    pub blob: BlobId,
}

/// All versioned files of a tree, keyed by path.
pub type FlatTree = BTreeMap<RepoPath, FileState>;

/// Whether scanning stores blobs or only hashes them.
#[derive(Clone, Copy)]
pub enum ScanMode<'a> {
    /// Compute ids only; never writes (used by `status`, `diff`).
    HashOnly,
    /// Durably store every blob (used by `save`).
    Store(&'a ObjectStore),
}

/// True for directory entries concurv never versions: `.concurv` at any level
/// (case-insensitive) and concurv's own init staging directories at the root.
pub fn is_ignored(rel: &RepoPath, name: &str) -> bool {
    name.eq_ignore_ascii_case(METADATA_DIR) || (rel.is_root() && name.starts_with(".concurv-init-"))
}

/// Scan the working copy under `root`. `head` supplies executable bits on
/// platforms without them (Windows).
pub fn scan(root: &Path, mode: ScanMode<'_>, head: &FlatTree) -> Result<FlatTree> {
    let mut out = FlatTree::new();
    scan_dir(root, &RepoPath::root(), mode, head, &mut out)?;
    Ok(out)
}

fn scan_dir(
    root: &Path,
    rel: &RepoPath,
    mode: ScanMode<'_>,
    head: &FlatTree,
    out: &mut FlatTree,
) -> Result<()> {
    let dir = rel.to_native(root);
    let rd = fs::read_dir(&dir).map_err(|e| ConcurvError::io(e, "read directory", &dir))?;
    let mut entries = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| ConcurvError::io(e, "read directory", &dir))?;
        let os_name = entry.file_name();
        let Some(name) = os_name.to_str() else {
            return Err(ConcurvError::new(
                ErrorCode::InvalidPath,
                format!(
                    "file name {:?} in `{}` is not valid UTF-8; concurv cannot version it yet (rename it)",
                    os_name,
                    dir.display()
                ),
            )
            .with_path(entry.path()));
        };
        entries.push(name.to_owned());
    }
    entries.sort();
    for name in entries {
        if is_ignored(rel, &name) {
            continue;
        }
        let path = rel.join(&name).map_err(|e| {
            ConcurvError::new(
                e.code,
                format!(
                    "cannot version `{}`: {}",
                    dir.join(&name).display(),
                    e.message
                ),
            )
        })?;
        let native = path.to_native(root);
        match stat_path(&native)? {
            Present::Missing => {} // deleted while scanning; not versioned
            Present::Dir => scan_dir(root, &path, mode, head, out)?,
            Present::File { len, exec } => {
                let kind = file_kind(exec, head.get(&path));
                let blob = read_file_blob(&native, len, mode)?;
                out.insert(path, FileState { kind, blob });
            }
            Present::Symlink(target) => {
                let blob = match mode {
                    ScanMode::HashOnly => blob_id(&target),
                    ScanMode::Store(s) => s.write_blob_bytes(&target)?,
                };
                out.insert(
                    path,
                    FileState {
                        kind: EntryKind::Symlink,
                        blob,
                    },
                );
            }
        }
    }
    Ok(())
}

fn file_kind(exec: Option<bool>, head: Option<&FileState>) -> EntryKind {
    match exec {
        Some(true) => EntryKind::Executable,
        Some(false) => EntryKind::File,
        // No executable bit on this platform: keep what the head recorded.
        None => match head {
            Some(FileState {
                kind: EntryKind::Executable,
                ..
            }) => EntryKind::Executable,
            _ => EntryKind::File,
        },
    }
}

/// What currently exists at a path (never follows symlinks).
#[derive(Debug, PartialEq, Eq)]
pub enum Present {
    /// Nothing.
    Missing,
    /// A real directory.
    Dir,
    /// A regular file; `exec` is `None` where the platform has no exec bit.
    File {
        /// Length in bytes.
        len: u64,
        /// Executable bit.
        exec: Option<bool>,
    },
    /// A symbolic link with its raw target bytes.
    Symlink(Vec<u8>),
}

/// Inspect a path without following symlinks; reject unsupported types.
pub fn stat_path(native: &Path) -> Result<Present> {
    let meta = match fs::symlink_metadata(native) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Present::Missing),
        Err(e) => return Err(ConcurvError::io(e, "inspect", native)),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        return symlink_target(native).map(Present::Symlink);
    }
    if ft.is_dir() {
        return Ok(Present::Dir);
    }
    if ft.is_file() {
        return Ok(Present::File {
            len: meta.len(),
            exec: exec_bit(&meta),
        });
    }
    Err(ConcurvError::new(
        ErrorCode::UnsupportedFileType,
        format!(
            "`{}` is not a regular file, directory or symlink (FIFO, socket or device); \
             remove it or move it outside the workspace",
            native.display()
        ),
    )
    .with_path(native))
}

#[cfg(unix)]
fn exec_bit(meta: &fs::Metadata) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn exec_bit(_meta: &fs::Metadata) -> Option<bool> {
    None
}

#[cfg(unix)]
fn symlink_target(native: &Path) -> Result<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let t = fs::read_link(native).map_err(|e| ConcurvError::io(e, "read symlink", native))?;
    let bytes = t.as_os_str().as_bytes().to_vec();
    if bytes.is_empty() {
        return Err(
            ConcurvError::new(ErrorCode::InvalidPath, "empty symlink target").with_path(native),
        );
    }
    Ok(bytes)
}

#[cfg(not(unix))]
fn symlink_target(native: &Path) -> Result<Vec<u8>> {
    Err(ConcurvError::new(
        ErrorCode::UnsupportedFileType,
        format!(
            "`{}` is a symlink or junction; concurv does not version links on Windows yet",
            native.display()
        ),
    )
    .with_path(native))
}

/// Hash (and optionally store) a regular file, guarding against the path
/// being swapped for a different file type between `stat` and `open`.
pub fn read_file_blob(native: &Path, len: u64, mode: ScanMode<'_>) -> Result<BlobId> {
    let file = File::open(native).map_err(|e| ConcurvError::io(e, "open", native))?;
    let meta = file
        .metadata()
        .map_err(|e| ConcurvError::io(e, "inspect", native))?;
    if !meta.is_file() || meta.len() != len {
        return Err(ConcurvError::new(
            ErrorCode::ConcurrentModification,
            format!("`{}` changed while being scanned; retry", native.display()),
        )
        .with_path(native));
    }
    match mode {
        ScanMode::HashOnly => hash_blob_stream(file, len, native),
        ScanMode::Store(s) => s.write_blob_stream(file, len, native),
    }
}

/// Current state of one path, hashed (used for re-checks before replacing).
pub fn current_state(root: &Path, path: &RepoPath, head: &FlatTree) -> Result<Option<FileState>> {
    let native = path.to_native(root);
    Ok(match stat_path(&native)? {
        Present::Missing | Present::Dir => None,
        Present::File { len, exec } => Some(FileState {
            kind: file_kind(exec, head.get(path)),
            blob: read_file_blob(&native, len, ScanMode::HashOnly)?,
        }),
        Present::Symlink(t) => Some(FileState {
            kind: EntryKind::Symlink,
            blob: blob_id(&t),
        }),
    })
}

// --------------------------------------------------------------- trees

#[derive(Default)]
struct DirNode {
    files: BTreeMap<String, FileState>,
    dirs: BTreeMap<String, DirNode>,
}

/// Write the trees for a flat map (blobs must already be stored); returns
/// the root tree id. Empty directories cannot occur since only files are listed.
pub fn write_trees(store: &ObjectStore, flat: &FlatTree) -> Result<TreeId> {
    let mut root = DirNode::default();
    for (path, state) in flat {
        let comps = path.components();
        let Some((last, dirs)) = comps.split_last() else {
            return Err(ConcurvError::internal("file entry with empty path"));
        };
        let mut node = &mut root;
        for d in dirs {
            node = node.dirs.entry(d.clone()).or_default();
        }
        node.files.insert(last.clone(), *state);
    }
    write_node(store, &root)
}

fn write_node(store: &ObjectStore, node: &DirNode) -> Result<TreeId> {
    let mut entries = Vec::with_capacity(node.files.len() + node.dirs.len());
    for (name, sub) in &node.dirs {
        let id = write_node(store, sub)?;
        entries.push(TreeEntry {
            name: name.clone(),
            kind: EntryKind::Tree,
            id: id.object_id(),
        });
    }
    for (name, st) in &node.files {
        entries.push(TreeEntry {
            name: name.clone(),
            kind: st.kind,
            id: st.blob.object_id(),
        });
    }
    store.write_tree(&Tree::new(entries)?)
}

/// Load a stored tree into a flat map, verifying every tree object and
/// structural rule. With `rules`, also refuse names that cannot be
/// materialized safely on this platform. Blobs are **not** verified here.
pub fn flatten(
    store: &ObjectStore,
    root: &TreeId,
    rules: Option<PlatformRules>,
) -> Result<FlatTree> {
    let mut out = FlatTree::new();
    flatten_into(store, root, &RepoPath::root(), rules, 0, &mut out)?;
    Ok(out)
}

fn flatten_into(
    store: &ObjectStore,
    id: &TreeId,
    rel: &RepoPath,
    rules: Option<PlatformRules>,
    depth: usize,
    out: &mut FlatTree,
) -> Result<()> {
    if depth > MAX_TREE_DEPTH {
        return Err(ConcurvError::new(
            ErrorCode::LimitExceeded,
            format!("tree nesting deeper than {MAX_TREE_DEPTH} at `{rel}`"),
        ));
    }
    let tree = store.read_tree(id)?;
    if tree.is_empty() && !rel.is_root() {
        return Err(ConcurvError::new(
            ErrorCode::ObjectCorrupt,
            format!("tree {id} at `{rel}` is an empty subdirectory, which is never written"),
        ));
    }
    if let Some(r) = rules {
        r.check_siblings(tree.entries().iter().map(|e| e.name.as_str()))
            .map_err(|e| in_dir(e, rel))?;
    }
    for e in tree.entries() {
        if let Some(r) = rules {
            r.check_name(&e.name).map_err(|err| in_dir(err, rel))?;
        }
        let path = rel.join(&e.name)?;
        if e.kind == EntryKind::Tree {
            flatten_into(
                store,
                &TreeId::from_object_id(e.id),
                &path,
                rules,
                depth + 1,
                out,
            )?;
        } else {
            out.insert(
                path,
                FileState {
                    kind: e.kind,
                    blob: BlobId::from_object_id(e.id),
                },
            );
        }
    }
    Ok(())
}

fn in_dir(mut e: ConcurvError, rel: &RepoPath) -> ConcurvError {
    if !rel.is_root() {
        e.message = format!("in `{rel}`: {}", e.message);
    }
    e
}
