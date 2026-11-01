//! Immutable, content-addressed loose-object store (RFC 0002 §3.1).

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use concurv_core::ids::is_lower_hex;
use concurv_core::{
    BlobId, ConcurvError, ErrorCode, ObjectId, OperationId, Result, SnapshotId, TreeId,
};

use crate::encoding::{
    HEADER_LEN, MAX_STRUCTURED_PAYLOAD, ObjectHasher, ObjectKind, Operation, Snapshot, Tree,
    decode_header, decode_object, encode_header, encode_object, hash_object,
};
use crate::fs::{Faults, TempFile, create_dir_durable, rename_durable};

const CHUNK: usize = 64 * 1024;

/// Result of scanning the object directory.
#[derive(Debug, Default)]
pub struct ObjectListing {
    /// Well-formed object file names.
    pub ids: Vec<ObjectId>,
    /// Files in the object directory whose names are not object ids.
    pub stray_files: Vec<PathBuf>,
}

/// Handle to `.concurv/objects` plus its temp and quarantine directories.
#[derive(Clone, Debug)]
pub struct ObjectStore {
    objects: PathBuf,
    tmp: PathBuf,
    quarantine: PathBuf,
    faults: Faults,
}

fn missing(id: &ObjectId) -> ConcurvError {
    ConcurvError::new(ErrorCode::ObjectMissing, format!("object {id} is missing"))
}

fn mismatch(id: &ObjectId, actual: &ObjectId) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::ObjectCorrupt,
        format!("object {id} is corrupt: content hashes to {actual}"),
    )
}

impl ObjectStore {
    /// Open a store rooted at an `.concurv` directory (layout must already exist).
    pub fn new(meta_dir: &Path, faults: Faults) -> Self {
        Self {
            objects: meta_dir.join("objects"),
            tmp: meta_dir.join("tmp"),
            quarantine: meta_dir.join("quarantine"),
            faults,
        }
    }

    /// Fault-injection handle in use.
    pub fn faults(&self) -> &Faults {
        &self.faults
    }

    /// Temporary-file directory.
    pub fn tmp_dir(&self) -> &Path {
        &self.tmp
    }

    /// Filesystem path of an object.
    pub fn path_for(&self, id: &ObjectId) -> PathBuf {
        let hex = id.to_hex();
        self.objects.join(&hex[..2]).join(&hex[2..])
    }

    /// True if an object file with this id exists (content not verified).
    pub fn contains(&self, id: &ObjectId) -> bool {
        fs::symlink_metadata(self.path_for(id)).is_ok_and(|m| m.is_file())
    }

    // ------------------------------------------------------------------ write

    /// Store a structured object or small blob given its payload.
    pub fn write_object(&self, kind: ObjectKind, payload: &[u8]) -> Result<ObjectId> {
        let bytes = encode_object(kind, payload)?;
        let id = hash_object(kind, payload);
        if self.dedup(&id)? {
            return Ok(id);
        }
        let mut t = TempFile::create(&self.tmp, "obj", &self.faults)?;
        for chunk in bytes.chunks(CHUNK) {
            t.write(chunk)?;
        }
        t.sync()?;
        self.promote(t, &id)?;
        Ok(id)
    }

    /// Store a blob from bytes.
    pub fn write_blob_bytes(&self, content: &[u8]) -> Result<BlobId> {
        self.write_object(ObjectKind::Blob, content)
            .map(BlobId::from_object_id)
    }

    /// Stream a blob from `reader`, which must yield exactly `len` bytes.
    /// A different byte count means the source changed while being read and
    /// fails with `CONCURV_CONCURRENT_MODIFICATION` (nothing is stored).
    pub fn write_blob_stream(
        &self,
        mut reader: impl Read,
        len: u64,
        what: &Path,
    ) -> Result<BlobId> {
        let mut t = TempFile::create(&self.tmp, "blob", &self.faults)?;
        let header = encode_header(ObjectKind::Blob, len);
        let mut hasher = ObjectHasher::new();
        hasher.update(&header);
        t.write(&header)?;
        let mut buf = vec![0u8; CHUNK];
        let mut total: u64 = 0;
        loop {
            let n =
                read_retry(&mut reader, &mut buf).map_err(|e| ConcurvError::io(e, "read", what))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > len {
                break;
            }
            hasher.update(&buf[..n]);
            t.write(&buf[..n])?;
        }
        if total != len {
            return Err(ConcurvError::new(
                ErrorCode::ConcurrentModification,
                format!(
                    "`{}` changed while being read (expected {len} bytes, read {total}); retry",
                    what.display()
                ),
            )
            .with_path(what));
        }
        t.sync()?;
        let id = hasher.finalize();
        if self.dedup(&id)? {
            return Ok(BlobId::from_object_id(id));
        }
        self.promote(t, &id)?;
        Ok(BlobId::from_object_id(id))
    }

    /// If a valid copy already exists return true; quarantine a corrupt copy.
    fn dedup(&self, id: &ObjectId) -> Result<bool> {
        if !self.contains(id) {
            return Ok(false);
        }
        match self.verify(id) {
            Ok(_) => Ok(true),
            Err(e)
                if matches!(
                    e.code,
                    ErrorCode::ObjectCorrupt
                        | ErrorCode::UnsupportedFormat
                        | ErrorCode::LimitExceeded
                ) =>
            {
                self.quarantine(id)?;
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    fn promote(&self, t: TempFile, id: &ObjectId) -> Result<()> {
        let dest = self.path_for(id);
        let fan = dest
            .parent()
            .ok_or_else(|| ConcurvError::internal("object path has no parent"))?;
        create_dir_durable(fan, &self.faults)?;
        t.persist(&dest)
    }

    /// Move a (corrupt) object out of the store for diagnosis.
    pub fn quarantine(&self, id: &ObjectId) -> Result<PathBuf> {
        create_dir_durable(&self.quarantine, &self.faults)?;
        let rid = concurv_core::RandomId::generate()?;
        let dest = self
            .quarantine
            .join(format!("{}.{}", id.to_hex(), rid.to_hex()));
        rename_durable(&self.path_for(id), &dest, &self.faults)?;
        Ok(dest)
    }

    // ------------------------------------------------------------------- read

    fn open(&self, id: &ObjectId) -> Result<(File, PathBuf)> {
        let path = self.path_for(id);
        match File::open(&path) {
            Ok(f) => Ok((f, path)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Err(missing(id)),
            Err(e) => Err(ConcurvError::io(e, "open object", &path)),
        }
    }

    /// Read a structured object, verifying hash, header, length and kind.
    pub fn read_object(&self, id: &ObjectId, kind: ObjectKind) -> Result<Vec<u8>> {
        let (file, path) = self.open(id)?;
        let mut bytes = Vec::new();
        file.take(HEADER_LEN as u64 + MAX_STRUCTURED_PAYLOAD + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| ConcurvError::io(e, "read object", &path))?;
        let (actual_kind, _) = decode_header(&bytes).map_err(|e| ctx(e, id))?;
        let payload = decode_object(&bytes, actual_kind).map_err(|e| ctx(e, id))?;
        let actual = hash_object(actual_kind, payload);
        if actual != *id {
            return Err(mismatch(id, &actual));
        }
        if actual_kind != kind {
            return Err(ConcurvError::new(
                ErrorCode::WrongObjectKind,
                format!(
                    "object {id} is a {}, not a {}",
                    actual_kind.name(),
                    kind.name()
                ),
            ));
        }
        Ok(payload.to_vec())
    }

    /// Stream a blob's content into `out`, verifying it. On error `out` may
    /// have received unverified bytes and must be discarded by the caller.
    pub fn read_blob_to(&self, id: &BlobId, out: &mut impl Write) -> Result<u64> {
        let oid = id.object_id();
        let (kind, len) = self.stream_verified(&oid, Some(out))?;
        if kind != ObjectKind::Blob {
            return Err(ConcurvError::new(
                ErrorCode::WrongObjectKind,
                format!("object {oid} is a {}, not a blob", kind.name()),
            ));
        }
        Ok(len)
    }

    /// Read a whole blob into memory (verified).
    pub fn read_blob(&self, id: &BlobId) -> Result<Vec<u8>> {
        let mut v = Vec::new();
        self.read_blob_to(id, &mut v)?;
        Ok(v)
    }

    /// Fully verify an object's bytes; returns its kind and payload length.
    pub fn verify(&self, id: &ObjectId) -> Result<(ObjectKind, u64)> {
        self.stream_verified(id, None::<&mut io::Sink>)
    }

    fn stream_verified<W: Write>(
        &self,
        id: &ObjectId,
        mut out: Option<&mut W>,
    ) -> Result<(ObjectKind, u64)> {
        let (mut file, path) = self.open(id)?;
        let mut header = [0u8; HEADER_LEN];
        let got = read_full(&mut file, &mut header)
            .map_err(|e| ConcurvError::io(e, "read object", &path))?;
        let (kind, len) = decode_header(&header[..got]).map_err(|e| ctx(e, id))?;
        let mut hasher = ObjectHasher::new();
        hasher.update(&header);
        let mut buf = vec![0u8; CHUNK];
        let mut total: u64 = 0;
        loop {
            let n = read_retry(&mut file, &mut buf)
                .map_err(|e| ConcurvError::io(e, "read object", &path))?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > len {
                return Err(ConcurvError::new(
                    ErrorCode::ObjectCorrupt,
                    format!("object {id} has trailing bytes beyond its declared length"),
                ));
            }
            hasher.update(&buf[..n]);
            if let Some(w) = out.as_deref_mut() {
                w.write_all(&buf[..n])
                    .map_err(|e| ConcurvError::io(e, "write content of object", &path))?;
            }
        }
        if total != len {
            return Err(ConcurvError::new(
                ErrorCode::ObjectCorrupt,
                format!("object {id} is truncated ({total} of {len} payload bytes)"),
            ));
        }
        let actual = hasher.finalize();
        if actual != *id {
            return Err(mismatch(id, &actual));
        }
        Ok((kind, len))
    }

    // ------------------------------------------------------------ typed APIs

    /// Store a tree.
    pub fn write_tree(&self, tree: &Tree) -> Result<TreeId> {
        self.write_object(ObjectKind::Tree, &tree.encode()?)
            .map(TreeId::from_object_id)
    }
    /// Load a verified tree.
    pub fn read_tree(&self, id: &TreeId) -> Result<Tree> {
        let p = self.read_object(&id.object_id(), ObjectKind::Tree)?;
        Tree::decode(&p).map_err(|e| ctx(e, &id.object_id()))
    }
    /// Store a snapshot.
    pub fn write_snapshot(&self, s: &Snapshot) -> Result<SnapshotId> {
        self.write_object(ObjectKind::Snapshot, &s.encode()?)
            .map(SnapshotId::from_object_id)
    }
    /// Load a verified snapshot.
    pub fn read_snapshot(&self, id: &SnapshotId) -> Result<Snapshot> {
        let p = self.read_object(&id.object_id(), ObjectKind::Snapshot)?;
        Snapshot::decode(&p).map_err(|e| ctx(e, &id.object_id()))
    }
    /// Store an operation.
    pub fn write_operation(&self, op: &Operation) -> Result<OperationId> {
        self.write_object(ObjectKind::Operation, &op.encode()?)
            .map(OperationId::from_object_id)
    }
    /// Load a verified operation.
    pub fn read_operation(&self, id: &OperationId) -> Result<Operation> {
        let p = self.read_object(&id.object_id(), ObjectKind::Operation)?;
        Operation::decode(&p).map_err(|e| ctx(e, &id.object_id()))
    }

    // -------------------------------------------------------------- listing

    /// List all object files (without verifying content).
    pub fn list(&self) -> Result<ObjectListing> {
        let mut out = ObjectListing::default();
        let rd = match fs::read_dir(&self.objects) {
            Ok(rd) => rd,
            Err(e) => return Err(ConcurvError::io(e, "list objects in", &self.objects)),
        };
        for fan in rd {
            let fan = fan.map_err(|e| ConcurvError::io(e, "list objects in", &self.objects))?;
            let fan_name = fan.file_name().to_string_lossy().into_owned();
            let fan_path = fan.path();
            if fan_name.len() != 2 || !is_lower_hex(&fan_name) || !fan_path.is_dir() {
                out.stray_files.push(fan_path);
                continue;
            }
            for f in fs::read_dir(&fan_path).map_err(|e| ConcurvError::io(e, "list", &fan_path))? {
                let f = f.map_err(|e| ConcurvError::io(e, "list", &fan_path))?;
                let full = format!("{fan_name}{}", f.file_name().to_string_lossy());
                match ObjectId::from_hex(&full) {
                    Ok(id) if f.path().is_file() => out.ids.push(id),
                    _ => out.stray_files.push(f.path()),
                }
            }
        }
        out.ids.sort();
        Ok(out)
    }

    /// Resolve a unique hex prefix (≥ 8 chars) to an object id.
    pub fn resolve_prefix(&self, prefix: &str) -> Result<ObjectId> {
        if prefix.len() == 64 {
            let id = ObjectId::from_hex(prefix)?;
            return if self.contains(&id) {
                Ok(id)
            } else {
                Err(missing(&id))
            };
        }
        if prefix.len() < 8 || prefix.len() > 64 || !is_lower_hex(prefix) {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "{prefix:?} is not an id or a lowercase hex prefix of at least 8 characters"
                ),
            ));
        }
        let fan = self.objects.join(&prefix[..2]);
        let mut found = Vec::new();
        if let Ok(rd) = fs::read_dir(&fan) {
            for f in rd.flatten() {
                let full = format!("{}{}", &prefix[..2], f.file_name().to_string_lossy());
                if full.starts_with(prefix)
                    && let Ok(id) = ObjectId::from_hex(&full)
                {
                    found.push(id);
                }
            }
        }
        match found.len() {
            0 => Err(ConcurvError::new(
                ErrorCode::NotFound,
                format!("no object matches prefix {prefix}"),
            )),
            1 => Ok(found[0]),
            n => Err(ConcurvError::new(
                ErrorCode::AmbiguousId,
                format!("prefix {prefix} matches {n} objects; use more characters"),
            )),
        }
    }
}

fn ctx(mut e: ConcurvError, id: &ObjectId) -> ConcurvError {
    e.message = format!("object {id}: {}", e.message);
    e
}

fn read_retry(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match r.read(buf) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut got = 0;
    while got < buf.len() {
        let n = read_retry(r, &mut buf[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    Ok(got)
}

/// Compute a blob id from a stream without storing it (used by `status`).
pub fn hash_blob_stream(mut reader: impl Read, len: u64, what: &Path) -> Result<BlobId> {
    let mut hasher = ObjectHasher::new();
    hasher.update(&encode_header(ObjectKind::Blob, len));
    let mut buf = vec![0u8; CHUNK];
    let mut total: u64 = 0;
    loop {
        let n = read_retry(&mut reader, &mut buf).map_err(|e| ConcurvError::io(e, "read", what))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > len {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if total != len {
        return Err(ConcurvError::new(
            ErrorCode::ConcurrentModification,
            format!("`{}` changed while being read; retry", what.display()),
        )
        .with_path(what));
    }
    Ok(BlobId::from_object_id(hasher.finalize()))
}

/// Create the empty store layout under `meta_dir` (used by `init`).
pub fn create_layout(meta_dir: &Path, faults: &Faults) -> Result<()> {
    for d in ["objects", "tmp", "quarantine"] {
        create_dir_durable(&meta_dir.join(d), faults)?;
    }
    Ok(())
}
