//! The relay: the single authority for the published branch heads of one
//! repository (RFC 0003 §1, §3.8).
//!
//! Layout of a relay directory:
//!
//! ```text
//! format          "concurv-relay-format 1" (written last by `init`)
//! objects/ tmp/ quarantine/   object store (RFC 0002 layout)
//! state.json      published heads, sequence numbers, recent publish results
//! devices.json    device registry: name, token hash, scopes
//! tls/cert.der tls/key.der    the relay's identity
//! lock            serializes changes of state.json and devices.json
//! publish.log     audit trail, one JSON line per publish (after the commit)
//! ```
//!
//! `state.json` is the relay's only mutable pointer: it is replaced
//! atomically (write, fsync, rename, fsync) under the lock, and that rename
//! is the commit point of a publish. It only ever names snapshots whose
//! whole graph is durable in `objects/`. Heads only move forward.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use concurv_core::{ConcurvError, ErrorCode, ObjectId, RandomId, Result, SnapshotId};
use concurv_objects::encoding::validate_branch_name;
use concurv_objects::fs::{
    FileLock, TempSession, clean_tmp_root, create_dir_durable, write_file_atomic,
};
use concurv_objects::{Faults, ObjectStore};
use concurv_workspace::graph::is_ancestor;
use concurv_workspace::refname;
use serde::{Deserialize, Serialize};

use crate::graph::{objects_to_send, staged_closure};
use crate::proto::{DEFAULT_MAX_BLOB, MAX_HAVES, MAX_WANTS};
use crate::tls::{Identity, fingerprint, generate_identity};

/// Contents of a relay's `format` file.
pub const RELAY_FORMAT: &str = "concurv-relay-format 1\n";
const STATE_FORMAT: &str = "concurv-relay-state 1";
const DEVICES_FORMAT: &str = "concurv-relay-devices 1";
/// Publish results remembered for retries with the same request id.
pub const REQUEST_MEMORY: usize = 1024;
const TOKEN_PREFIX: &str = "cvt1_";
const TOKEN_CONTEXT: &str = "concurv 2026-11 relay device token v1";

/// Options of an open relay.
#[derive(Clone, Debug)]
pub struct RelayOptions {
    /// Fault injection for durable writes (tests).
    pub faults: Faults,
    /// Largest blob accepted.
    pub max_blob: u64,
    /// Wait for the relay lock.
    pub lock_timeout: Duration,
    /// Fixed clock (tests).
    pub now_ms: Option<i64>,
}

impl Default for RelayOptions {
    fn default() -> Self {
        Self {
            faults: Faults::none(),
            max_blob: DEFAULT_MAX_BLOB,
            lock_timeout: Duration::from_secs(30),
            now_ms: None,
        }
    }
}

/// What a device may do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// List heads and download objects.
    Read,
    /// Upload objects.
    Write,
    /// Publish to one branch, or to every branch (`None`, written `publish:*`).
    Publish(Option<String>),
}

impl Scope {
    /// Parse `read`, `write`, `publish:*` or `publish:<branch>`.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "publish:*" => Ok(Self::Publish(None)),
            _ => match s.strip_prefix("publish:") {
                Some(b) if refname::git_refname_ok(b) && refname::portable(b).is_ok() => {
                    Ok(Self::Publish(Some(b.to_owned())))
                }
                _ => Err(ConcurvError::new(
                    ErrorCode::InvalidArgument,
                    format!("invalid scope {s:?}: use read, write, publish:* or publish:<branch>"),
                )),
            },
        }
    }
}

impl std::fmt::Display for Scope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read => f.write_str("read"),
            Self::Write => f.write_str("write"),
            Self::Publish(None) => f.write_str("publish:*"),
            Self::Publish(Some(b)) => write!(f, "publish:{b}"),
        }
    }
}

/// An authenticated device.
#[derive(Clone, Debug)]
pub struct Device {
    /// Its name in the registry.
    pub name: String,
    /// Its scopes.
    pub scopes: Vec<Scope>,
}

impl Device {
    fn denied(&self, what: &str) -> ConcurvError {
        ConcurvError::new(
            ErrorCode::PermissionDenied,
            format!("device {:?} may not {what}", self.name),
        )
    }

    /// Require the read scope.
    pub fn require_read(&self) -> Result<()> {
        if self.scopes.contains(&Scope::Read) {
            Ok(())
        } else {
            Err(self.denied("read from this relay (scope `read`)"))
        }
    }

    /// Require the write scope.
    pub fn require_write(&self) -> Result<()> {
        if self.scopes.contains(&Scope::Write) {
            Ok(())
        } else {
            Err(self.denied("upload objects (scope `write`)"))
        }
    }

    /// Require permission to publish `branch`.
    pub fn require_publish(&self, branch: &str) -> Result<()> {
        self.require_write()?;
        let ok = self.scopes.iter().any(|s| match s {
            Scope::Publish(None) => true,
            Scope::Publish(Some(b)) => b == branch,
            _ => false,
        });
        if ok {
            Ok(())
        } else {
            Err(self.denied(&format!(
                "publish branch {branch:?} (scope `publish:{branch}` or `publish:*`)"
            )))
        }
    }
}

/// A device as stored in `devices.json` (the token itself is never stored).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRecord {
    /// Name.
    pub name: String,
    /// Keyed BLAKE3 hash of the token, hex.
    pub token_hash: String,
    /// Scopes.
    pub scopes: Vec<String>,
    /// When it was added (ms since the epoch).
    pub created_ms: i64,
    /// Revoked devices can no longer authenticate.
    pub revoked: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DevicesFile {
    format: String,
    devices: Vec<DeviceRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadRecord {
    head: String,
    seq: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestRecord {
    id: String,
    device: String,
    branch: String,
    head: String,
    seq: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFile {
    format: String,
    relay: String,
    branches: BTreeMap<String, HeadRecord>,
    requests: Vec<RequestRecord>,
}

/// A published head as the relay holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    /// Head snapshot.
    pub head: SnapshotId,
    /// Publication sequence number.
    pub seq: u64,
}

/// Validated relay state.
#[derive(Clone, Debug)]
struct State {
    relay: String,
    branches: BTreeMap<String, Head>,
    requests: Vec<RequestRecord>,
}

fn damaged(what: &str, path: &Path, why: impl std::fmt::Display) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::ObjectCorrupt,
        format!("relay {what} is damaged: {why}"),
    )
    .with_path(path)
}

impl State {
    fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(|e| ConcurvError::io(e, "read", path))?;
        let f: StateFile = serde_json::from_slice(&bytes).map_err(|e| damaged("state", path, e))?;
        if f.format != STATE_FORMAT || RandomId::from_hex(&f.relay).is_err() {
            return Err(damaged("state", path, "unknown format or relay id"));
        }
        let mut branches = BTreeMap::new();
        for (name, h) in f.branches {
            validate_branch_name(&name).map_err(|e| damaged("state", path, e))?;
            let head = ObjectId::from_hex(&h.head)
                .map(SnapshotId::from_object_id)
                .map_err(|e| damaged("state", path, e))?;
            if h.seq == 0 {
                return Err(damaged("state", path, "sequence number 0"));
            }
            branches.insert(name, Head { head, seq: h.seq });
        }
        Ok(Self {
            relay: f.relay,
            branches,
            requests: f.requests,
        })
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let f = StateFile {
            format: STATE_FORMAT.into(),
            relay: self.relay.clone(),
            branches: self
                .branches
                .iter()
                .map(|(n, h)| {
                    (
                        n.clone(),
                        HeadRecord {
                            head: h.head.to_hex(),
                            seq: h.seq,
                        },
                    )
                })
                .collect(),
            requests: self.requests.clone(),
        };
        serde_json::to_vec_pretty(&f)
            .map_err(|e| ConcurvError::internal(format!("cannot encode relay state: {e}")))
    }
}

/// Result of [`Relay::init`].
#[derive(Debug)]
pub struct InitOutcome {
    /// The relay's id.
    pub relay: String,
    /// Fingerprint clients must pin.
    pub fingerprint: String,
}

/// Request to publish.
#[derive(Clone, Debug)]
pub struct PublishRequest {
    /// Client-chosen request id (hex of 16 random bytes).
    pub request_id: String,
    /// Branch.
    pub branch: String,
    /// Head the client expects the relay to have.
    pub expected: Option<SnapshotId>,
    /// New head.
    pub proposed: SnapshotId,
}

/// Result of a successful publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Published {
    /// Branch.
    pub branch: String,
    /// New head.
    pub head: SnapshotId,
    /// Its sequence number.
    pub seq: u64,
    /// Repeats an earlier result for the same request id.
    pub replayed: bool,
}

/// Why a publish was refused, with details for the client.
#[derive(Debug)]
pub struct PublishError {
    /// The error.
    pub error: ConcurvError,
    /// For a stale publish: the relay's head.
    pub observed: Option<Head>,
    /// For an incomplete graph: missing objects.
    pub missing: Vec<ObjectId>,
}

impl From<ConcurvError> for Box<PublishError> {
    fn from(error: ConcurvError) -> Self {
        Box::new(PublishError {
            error,
            observed: None,
            missing: Vec::new(),
        })
    }
}

/// A connection's private staging area for uploaded objects (RFC 0003
/// §3.6): a temp session in the relay's `tmp/`, deleted when dropped and
/// by recovery if the relay died.
#[derive(Debug)]
pub struct Session {
    _tmp: TempSession,
    staging: ObjectStore,
}

impl Session {
    /// The staging store.
    pub fn staging(&self) -> &ObjectStore {
        &self.staging
    }
}

/// An open relay directory.
#[derive(Debug)]
pub struct Relay {
    dir: PathBuf,
    store: ObjectStore,
    opts: RelayOptions,
    mutex: Mutex<()>,
}

fn now(opts: &RelayOptions) -> i64 {
    opts.now_ms.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or(0)
    })
}

fn token_hash(token: &[u8]) -> blake3::Hash {
    blake3::Hasher::new_derive_key(TOKEN_CONTEXT)
        .update(token)
        .finalize()
}

fn parse_token(token: &str) -> Option<[u8; 32]> {
    let hex = token.strip_prefix(TOKEN_PREFIX)?;
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

#[cfg(unix)]
fn write_private(tmp: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let t = tmp.join(format!("key.{}.tmp", RandomId::generate()?.to_hex()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&t)
        .map_err(|e| ConcurvError::io(e, "create", &t))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| ConcurvError::io(e, "write", &t))?;
    drop(f);
    concurv_objects::fs::rename_durable(&t, dest, &Faults::none())
}

#[cfg(not(unix))]
fn write_private(tmp: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    write_file_atomic(tmp, dest, bytes, &Faults::none())
}

impl Relay {
    /// Create a relay in `dir` (which must not exist or be empty): object
    /// store, empty state and device registry, and a new TLS identity.
    pub fn init(dir: &Path) -> Result<InitOutcome> {
        if dir.join("format").exists() {
            return Err(ConcurvError::new(
                ErrorCode::AlreadyInitialized,
                format!("`{}` already contains a relay", dir.display()),
            ));
        }
        match fs::read_dir(dir) {
            Ok(mut rd) => {
                if rd.next().is_some() {
                    return Err(ConcurvError::new(
                        ErrorCode::InvalidArgument,
                        format!("`{}` is not empty", dir.display()),
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(dir).map_err(|e| ConcurvError::io(e, "create", dir))?;
            }
            Err(e) => return Err(ConcurvError::io(e, "inspect", dir)),
        }
        let faults = Faults::none();
        concurv_objects::store::create_layout(dir, &faults)?;
        let tmp = dir.join("tmp");
        create_dir_durable(&dir.join("tls"), &faults)?;
        let id = generate_identity()?;
        write_file_atomic(&tmp, &dir.join("tls").join("cert.der"), &id.cert, &faults)?;
        write_private(&tmp, &dir.join("tls").join("key.der"), &id.key)?;
        let relay = RandomId::generate()?.to_hex();
        let state = State {
            relay: relay.clone(),
            branches: BTreeMap::new(),
            requests: Vec::new(),
        };
        write_file_atomic(&tmp, &dir.join("state.json"), &state.encode()?, &faults)?;
        let devices = DevicesFile {
            format: DEVICES_FORMAT.into(),
            devices: Vec::new(),
        };
        let bytes = serde_json::to_vec_pretty(&devices)
            .map_err(|e| ConcurvError::internal(e.to_string()))?;
        write_file_atomic(&tmp, &dir.join("devices.json"), &bytes, &faults)?;
        write_file_atomic(&tmp, &dir.join("format"), RELAY_FORMAT.as_bytes(), &faults)?;
        Ok(InitOutcome {
            relay,
            fingerprint: fingerprint(&id.cert),
        })
    }

    /// Open an existing relay, validate its state and remove debris of
    /// interrupted sessions.
    pub fn open(dir: &Path, opts: RelayOptions) -> Result<Self> {
        let fpath = dir.join("format");
        let format = match fs::read_to_string(&fpath) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ConcurvError::new(
                    ErrorCode::NotARepo,
                    format!("`{}` is not a relay (run `concurvd init`)", dir.display()),
                ));
            }
            Err(e) => return Err(ConcurvError::io(e, "read", &fpath)),
        };
        if format != RELAY_FORMAT {
            return Err(ConcurvError::new(
                ErrorCode::UnsupportedFormat,
                format!(
                    "relay format {:?} is not supported by this build",
                    format.trim()
                ),
            )
            .with_path(&fpath));
        }
        let relay = Self {
            dir: dir.to_path_buf(),
            store: ObjectStore::new(dir, opts.faults.clone()),
            opts,
            mutex: Mutex::new(()),
        };
        State::load(&relay.dir.join("state.json"))?;
        relay.load_devices()?;
        {
            let _l = relay.lock()?;
            clean_tmp_root(relay.store.tmp_dir())?;
        }
        Ok(relay)
    }

    /// The relay directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The relay's object store.
    pub fn store(&self) -> &ObjectStore {
        &self.store
    }

    /// Options in use.
    pub fn options(&self) -> &RelayOptions {
        &self.opts
    }

    fn lock(&self) -> Result<(MutexGuard<'_, ()>, FileLock)> {
        let g = self
            .mutex
            .lock()
            .map_err(|_| ConcurvError::internal("relay lock poisoned"))?;
        let f = FileLock::acquire(
            &self.dir.join("lock"),
            self.opts.lock_timeout,
            "the relay lock",
        )?;
        Ok((g, f))
    }

    /// The relay's TLS identity.
    pub fn identity(&self) -> Result<Identity> {
        let tls = self.dir.join("tls");
        let read = |n: &str| {
            let p = tls.join(n);
            fs::read(&p).map_err(|e| ConcurvError::io(e, "read", &p))
        };
        Ok(Identity {
            cert: read("cert.der")?,
            key: read("key.der")?,
        })
    }

    /// Fingerprint clients pin.
    pub fn fingerprint(&self) -> Result<String> {
        Ok(fingerprint(&self.identity()?.cert))
    }

    /// The relay's id.
    pub fn relay_id(&self) -> Result<String> {
        Ok(State::load(&self.dir.join("state.json"))?.relay)
    }

    // --------------------------------------------------------------- devices

    fn load_devices(&self) -> Result<Vec<DeviceRecord>> {
        let p = self.dir.join("devices.json");
        let bytes = fs::read(&p).map_err(|e| ConcurvError::io(e, "read", &p))?;
        let f: DevicesFile =
            serde_json::from_slice(&bytes).map_err(|e| damaged("device registry", &p, e))?;
        if f.format != DEVICES_FORMAT {
            return Err(damaged("device registry", &p, "unknown format"));
        }
        Ok(f.devices)
    }

    fn save_devices(&self, devices: Vec<DeviceRecord>) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&DevicesFile {
            format: DEVICES_FORMAT.into(),
            devices,
        })
        .map_err(|e| ConcurvError::internal(e.to_string()))?;
        write_file_atomic(
            self.store.tmp_dir(),
            &self.dir.join("devices.json"),
            &bytes,
            &self.opts.faults,
        )
    }

    /// Register a device and return its token. The token is shown once:
    /// only a keyed hash of it is stored.
    pub fn add_device(&self, name: &str, scopes: &[Scope]) -> Result<String> {
        concurv_objects::encoding::validate_workspace_name(name).map_err(|_| {
            ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "invalid device name {name:?}: use 1-64 characters of A-Z a-z 0-9 . _ -, not starting with . or -"
                ),
            )
        })?;
        let _l = self.lock()?;
        let mut devices = self.load_devices()?;
        if devices.iter().any(|d| d.name == name) {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!("a device named {name:?} already exists"),
            ));
        }
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|e| ConcurvError::internal(format!("OS random generator failed: {e}")))?;
        let mut token = String::from(TOKEN_PREFIX);
        for b in secret {
            token.push_str(&format!("{b:02x}"));
        }
        devices.push(DeviceRecord {
            name: name.to_owned(),
            token_hash: token_hash(&secret).to_hex().to_string(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            created_ms: now(&self.opts),
            revoked: false,
        });
        self.save_devices(devices)?;
        Ok(token)
    }

    /// Revoke a device: its token stops working at its next connection.
    pub fn revoke_device(&self, name: &str) -> Result<()> {
        let _l = self.lock()?;
        let mut devices = self.load_devices()?;
        let d = devices.iter_mut().find(|d| d.name == name).ok_or_else(|| {
            ConcurvError::new(ErrorCode::NotFound, format!("no device named {name:?}"))
        })?;
        d.revoked = true;
        self.save_devices(devices)
    }

    /// The device registry.
    pub fn devices(&self) -> Result<Vec<DeviceRecord>> {
        self.load_devices()
    }

    /// Check a token. Every failure is the same `CONCURV_AUTH_FAILED`.
    pub fn authenticate(&self, token: &str) -> Result<Device> {
        let fail = || ConcurvError::new(ErrorCode::AuthFailed, "unknown or revoked device token");
        let secret = parse_token(token).ok_or_else(fail)?;
        let h = token_hash(&secret);
        let mut found = None;
        for d in self.load_devices()? {
            // `blake3::Hash` compares in constant time.
            if blake3::Hash::from_hex(&d.token_hash).is_ok_and(|s| s == h) {
                found = Some(d);
            }
        }
        match found {
            Some(d) if !d.revoked => Ok(Device {
                scopes: d
                    .scopes
                    .iter()
                    .map(|s| Scope::parse(s))
                    .collect::<Result<Vec<_>>>()?,
                name: d.name,
            }),
            _ => Err(fail()),
        }
    }

    // ------------------------------------------------------------- branches

    /// Published heads.
    pub fn heads(&self) -> Result<BTreeMap<String, Head>> {
        Ok(State::load(&self.dir.join("state.json"))?.branches)
    }

    /// Start a staging session for uploads.
    pub fn session(&self) -> Result<Session> {
        let tmp = {
            let _l = self.lock()?;
            TempSession::create(self.store.tmp_dir(), &self.opts.faults)?
        };
        concurv_objects::store::create_layout(tmp.dir(), &self.opts.faults)?;
        let staging = ObjectStore::new(tmp.dir(), self.opts.faults.clone());
        Ok(Session { _tmp: tmp, staging })
    }

    /// Objects for a `fetch` request.
    pub fn objects_for(&self, wants: &[SnapshotId], haves: &[SnapshotId]) -> Result<Vec<ObjectId>> {
        if wants.len() > MAX_WANTS || haves.len() > MAX_HAVES {
            return Err(crate::proto::protocol("too many wants or haves"));
        }
        for w in wants {
            if !self.store.contains(&w.object_id()) {
                return Err(ConcurvError::new(
                    ErrorCode::NotFound,
                    format!("the relay has no snapshot {w}"),
                ));
            }
        }
        objects_to_send(&self.store, wants, haves)
    }

    /// The subset of `ids` the relay lacks.
    pub fn missing(&self, ids: &[ObjectId]) -> Vec<ObjectId> {
        ids.iter()
            .filter(|id| !self.store.contains(id))
            .copied()
            .collect()
    }

    /// Compare-and-swap of a published head (RFC 0003 §3.8). Under the
    /// relay lock: a retried request returns its first result; the branch
    /// must still be at `expected`; the proposed snapshot's graph must be
    /// complete in the store or `staging`, whose objects are then adopted
    /// in dependency order; the proposal must descend from the current head
    /// (heads only move forward). Then `state.json` is replaced: the commit
    /// point.
    pub fn publish(
        &self,
        device: &Device,
        staging: Option<&ObjectStore>,
        req: &PublishRequest,
    ) -> std::result::Result<Published, Box<PublishError>> {
        device.require_publish(&req.branch)?;
        if RandomId::from_hex(&req.request_id).is_err() {
            return Err(crate::proto::protocol("request_id must be 32 hex digits").into());
        }
        let _l = self.lock()?;
        let path = self.dir.join("state.json");
        let mut state = State::load(&path)?;
        if let Some(r) = state.requests.iter().find(|r| r.id == req.request_id) {
            if r.branch != req.branch || r.head != req.proposed.to_hex() || r.device != device.name
            {
                return Err(crate::proto::protocol(
                    "request_id was already used for a different publish",
                )
                .into());
            }
            return Ok(Published {
                branch: r.branch.clone(),
                head: req.proposed,
                seq: r.seq,
                replayed: true,
            });
        }
        let current = state.branches.get(&req.branch).copied();
        if current.map(|h| h.head) != req.expected {
            return Err(Box::new(PublishError {
                error: ConcurvError::new(
                    ErrorCode::StaleHead,
                    format!(
                        "branch {} moved since you last fetched; fetch, integrate and push again",
                        req.branch
                    ),
                ),
                observed: current,
                missing: Vec::new(),
            }));
        }
        if current.is_none() {
            validate_branch_name(&req.branch)?;
            let existing: Vec<&String> = state.branches.keys().collect();
            refname::check_new(&req.branch, &existing)?;
        }
        let empty;
        let staging = match staging {
            Some(s) => s,
            None => {
                empty = ObjectStore::new(&self.dir.join("tmp").join("none"), Faults::none());
                &empty
            }
        };
        let closure = staged_closure(staging, &self.store, &[req.proposed])?;
        if !closure.missing.is_empty() {
            return Err(Box::new(PublishError {
                error: ConcurvError::new(
                    ErrorCode::ObjectMissing,
                    format!(
                        "the history of {} is incomplete on the relay: upload its objects first",
                        req.proposed
                    ),
                ),
                observed: None,
                missing: closure.missing,
            }));
        }
        for id in &closure.order {
            self.store.adopt(staging, id)?;
        }
        if let Some(c) = current
            && !is_ancestor(&self.store, c.head, req.proposed)?
        {
            return Err(ConcurvError::new(
                ErrorCode::Diverged,
                format!(
                    "{} does not descend from the head of {}: published heads only move forward",
                    req.proposed, req.branch
                ),
            )
            .into());
        }
        let seq = current.map_or(1, |c| c.seq + 1);
        state.branches.insert(
            req.branch.clone(),
            Head {
                head: req.proposed,
                seq,
            },
        );
        state.requests.push(RequestRecord {
            id: req.request_id.clone(),
            device: device.name.clone(),
            branch: req.branch.clone(),
            head: req.proposed.to_hex(),
            seq,
        });
        let excess = state.requests.len().saturating_sub(REQUEST_MEMORY);
        state.requests.drain(..excess);
        write_file_atomic(
            self.store.tmp_dir(),
            &path,
            &state.encode()?,
            &self.opts.faults,
        )?;
        self.audit(device, req, current, seq);
        Ok(Published {
            branch: req.branch.clone(),
            head: req.proposed,
            seq,
            replayed: false,
        })
    }

    /// Append to `publish.log`. After the commit point and best effort: the
    /// log is an audit trail, never read back as state.
    fn audit(&self, device: &Device, req: &PublishRequest, from: Option<Head>, seq: u64) {
        let line = serde_json::json!({
            "time_ms": now(&self.opts),
            "device": device.name,
            "branch": req.branch,
            "from": from.map(|h| h.head.to_hex()),
            "to": req.proposed.to_hex(),
            "seq": seq,
            "request_id": req.request_id,
        });
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("publish.log"))
        {
            let _ = writeln!(f, "{line}").and_then(|()| f.sync_data());
        }
    }
}
