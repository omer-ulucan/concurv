//! Wire protocol, version 1 (RFC 0003 §3).
//!
//! After the TLS handshake both sides exchange frames: a one-byte frame type,
//! a big-endian `u64` length, then that many bytes. A *message* frame holds
//! one JSON object (at most [`MAX_MESSAGE`] bytes); an *object* frame holds
//! one object in its canonical encoding (RFC 0001 §3), whose id the receiver
//! computes itself. The client sends requests; the relay answers each with
//! one message, which may announce object frames that follow it.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};

use concurv_core::{ConcurvError, ErrorCode, ObjectId, Result, SnapshotId};
use concurv_objects::{ObjectKind, ObjectStore};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Protocol name sent in `hello`.
pub const PROTOCOL: &str = "concurv-sync";
/// The protocol version this build speaks.
pub const VERSION: u32 = 1;
/// The object format version this build stores (RFC 0001).
pub const OBJECT_FORMAT: u32 = 1;
/// Default TCP port of a relay.
pub const DEFAULT_PORT: u16 = 7461;
/// Largest message frame.
pub const MAX_MESSAGE: u64 = 1 << 20;
/// Default largest blob either side accepts.
pub const DEFAULT_MAX_BLOB: u64 = 4 << 30;
/// Most snapshot ids in one `fetch` request's `wants`.
pub const MAX_WANTS: usize = 1024;
/// Most snapshot ids in one `fetch` request's `haves`.
pub const MAX_HAVES: usize = 4096;
/// Most ids in one `missing` query.
pub const MAX_QUERY_IDS: usize = 4096;
/// Most objects announced by one `upload` request.
pub const MAX_UPLOAD_BATCH: u32 = 10_000;
/// Most objects a client accepts in one `pack` answer.
pub const MAX_PACK: u32 = 10_000_000;

const FRAME_MESSAGE: u8 = 1;
const FRAME_OBJECT: u8 = 2;

/// A client request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// First message: protocol negotiation and the device's credentials.
    Hello {
        /// Always [`PROTOCOL`].
        protocol: String,
        /// Versions the client speaks.
        versions: Vec<u32>,
        /// Object format the client stores.
        object_format: u32,
        /// Device token issued by the relay's administrator.
        token: String,
    },
    /// List published branch heads.
    Heads {},
    /// Send every object reachable from `wants` that is not known to be
    /// reachable from `haves` (snapshot ids the client already has).
    Fetch {
        /// Snapshots wanted.
        wants: Vec<String>,
        /// Snapshots the client has, with their whole history.
        haves: Vec<String>,
    },
    /// Which of these objects does the relay lack?
    Missing {
        /// Object ids.
        ids: Vec<String>,
    },
    /// Announce `count` object frames. The relay answers `ready` (or an
    /// error, and nothing is sent); then the frames follow, and the relay
    /// keeps them in this session's quarantine until a publish needs them.
    Upload {
        /// Number of object frames that follow.
        count: u32,
    },
    /// Compare-and-swap of a published branch head (RFC 0003 §3.8).
    Publish {
        /// Client-chosen id; a retry with the same id returns the original
        /// result instead of publishing twice.
        request_id: String,
        /// Branch name.
        branch: String,
        /// Head the client last observed (`null`: the branch must not exist).
        expected: Option<String>,
        /// New head; must descend from `expected`.
        proposed: String,
        /// For a branch the relay's policy protects (RFC 0005): an attestation
        /// submitted with `attest`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attestation: Option<String>,
    },
    /// Vouch for an attestation object (uploaded earlier in this session, or
    /// already stored): the relay records that this device, which needs the
    /// `attest` scope, submitted it (RFC 0005 §7).
    Attest {
        /// Attestation id.
        id: String,
    },
    /// End of session.
    Bye {},
}

/// A relay's published head of one branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadInfo {
    /// Head snapshot.
    pub head: String,
    /// Publication sequence number of the branch (1 for its first publish).
    pub seq: u64,
}

/// A relay response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    /// Answer to `hello`.
    Welcome {
        /// Negotiated protocol version.
        version: u32,
        /// The relay's id.
        relay: String,
        /// Name of the authenticated device.
        device: String,
        /// Its scopes.
        scopes: Vec<String>,
        /// Largest blob the relay accepts.
        max_blob: u64,
    },
    /// Answer to `heads`.
    Heads {
        /// Branch → head.
        branches: BTreeMap<String, HeadInfo>,
    },
    /// Answer to `fetch`: `count` object frames follow.
    Pack {
        /// Number of object frames that follow.
        count: u32,
    },
    /// Answer to `missing`.
    Missing {
        /// The subset of the queried ids the relay lacks.
        ids: Vec<String>,
    },
    /// First answer to `upload`: the relay accepts the batch; send the
    /// object frames now. (A refusal is an `error` instead, before any
    /// object is sent.)
    Ready {},
    /// Final answer to `upload`, after every object was received.
    Stored {
        /// Objects received and verified.
        received: u32,
    },
    /// Answer to `attest`.
    Attested {
        /// Attestation id.
        id: String,
    },
    /// Answer to a successful `publish`.
    Published {
        /// Branch.
        branch: String,
        /// Its new head.
        head: String,
        /// Its publication sequence number.
        seq: u64,
        /// True if this repeats the result of an earlier request with the
        /// same `request_id`.
        replayed: bool,
    },
    /// The request failed. `code` is a stable error code (docs/cli.md).
    Error {
        /// Stable error code, e.g. `CONCURV_STALE_HEAD`.
        code: String,
        /// Explanation.
        message: String,
        /// For a stale publish: the head the relay has.
        #[serde(default)]
        observed: Option<String>,
        /// For a stale publish: its sequence number.
        #[serde(default)]
        seq: Option<u64>,
        /// For a publish whose graph is incomplete: missing objects.
        #[serde(default)]
        missing: Vec<String>,
    },
}

impl Response {
    /// An error response for `e`.
    pub fn error(e: &ConcurvError) -> Self {
        Response::Error {
            code: e.code.as_str().to_owned(),
            message: e.message.clone(),
            observed: None,
            seq: None,
            missing: Vec::new(),
        }
    }
}

/// A connection-level error with code `CONCURV_NETWORK`.
pub fn network(e: &io::Error, what: &str) -> ConcurvError {
    ConcurvError::new(ErrorCode::Network, format!("{what}: {e}"))
}

/// A `CONCURV_PROTOCOL` error.
pub fn protocol(msg: impl Into<String>) -> ConcurvError {
    ConcurvError::new(ErrorCode::Protocol, msg)
}

fn read_header(r: &mut impl Read) -> Result<(u8, u64)> {
    let mut h = [0u8; 9];
    r.read_exact(&mut h)
        .map_err(|e| network(&e, "connection lost"))?;
    let mut len = [0u8; 8];
    len.copy_from_slice(&h[1..]);
    Ok((h[0], u64::from_be_bytes(len)))
}

fn write_header(w: &mut impl Write, kind: u8, len: u64) -> Result<()> {
    let mut h = [0u8; 9];
    h[0] = kind;
    h[1..].copy_from_slice(&len.to_be_bytes());
    w.write_all(&h).map_err(|e| network(&e, "connection lost"))
}

/// Send one message frame.
pub fn write_message(w: &mut impl Write, msg: &impl Serialize) -> Result<()> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| ConcurvError::internal(format!("cannot encode message: {e}")))?;
    write_header(w, FRAME_MESSAGE, body.len() as u64)?;
    w.write_all(&body)
        .map_err(|e| network(&e, "connection lost"))?;
    w.flush().map_err(|e| network(&e, "connection lost"))
}

/// Read one message frame and decode it as `T`. Anything else (an object
/// frame, an oversized frame, malformed or unknown JSON) is a protocol error.
pub fn read_message<T: DeserializeOwned>(r: &mut impl Read) -> Result<T> {
    let (kind, len) = read_header(r)?;
    if kind != FRAME_MESSAGE {
        return Err(protocol(format!(
            "expected a message frame, got type {kind}"
        )));
    }
    if len > MAX_MESSAGE {
        return Err(protocol(format!(
            "message of {len} bytes exceeds the limit of {MAX_MESSAGE}"
        )));
    }
    let mut body = vec![0u8; usize::try_from(len).unwrap_or(0)];
    r.read_exact(&mut body)
        .map_err(|e| network(&e, "connection lost"))?;
    serde_json::from_slice(&body).map_err(|e| protocol(format!("malformed message: {e}")))
}

/// Send one object frame, streaming it from `store`.
pub fn write_object(w: &mut impl Write, store: &ObjectStore, id: &ObjectId) -> Result<()> {
    let (mut file, len) = store.open_raw(id)?;
    write_header(w, FRAME_OBJECT, len)?;
    let copied = io::copy(&mut (&mut file).take(len), w).map_err(|e| network(&e, "send"))?;
    if copied != len {
        return Err(ConcurvError::new(
            ErrorCode::ObjectCorrupt,
            format!("object {id} changed size while being sent"),
        ));
    }
    Ok(())
}

/// A reader that remembers whether the underlying stream failed, so errors
/// of the transport can be told apart from errors of the content.
struct Tracked<'a, R> {
    inner: &'a mut R,
    failed: Option<String>,
}

impl<R: Read> Read for Tracked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(buf) {
            Ok(0) if !buf.is_empty() => {
                self.failed = Some("connection closed".into());
                Ok(0)
            }
            Err(e) => {
                self.failed = Some(e.to_string());
                Err(e)
            }
            ok => ok,
        }
    }
}

/// Receive one object frame into `staging` (RFC 0003 §3.6): the content is
/// verified and the id computed from the bytes. A transport failure is a
/// `CONCURV_NETWORK` error; bad content keeps its integrity error code.
pub fn read_object(
    r: &mut impl Read,
    staging: &ObjectStore,
    max_blob: u64,
) -> Result<(ObjectId, ObjectKind)> {
    let (kind, len) = read_header(r)?;
    if kind != FRAME_OBJECT {
        return Err(protocol(format!(
            "expected an object frame, got type {kind}"
        )));
    }
    let mut t = Tracked {
        inner: r,
        failed: None,
    };
    staging
        .receive_object(&mut t, len, max_blob)
        .map_err(|e| match t.failed.take() {
            Some(why) if e.code == ErrorCode::Io => {
                ConcurvError::new(ErrorCode::Network, format!("connection lost: {why}"))
            }
            _ => e,
        })
}

/// Parse a hex object id received from a peer.
pub fn parse_id(s: &str) -> Result<ObjectId> {
    ObjectId::from_hex(s).map_err(|_| protocol(format!("invalid object id {s:?}")))
}

/// Parse a hex snapshot id received from a peer.
pub fn parse_snapshot(s: &str) -> Result<SnapshotId> {
    parse_id(s).map(SnapshotId::from_object_id)
}

/// Parse a list of ids, refusing more than `max`.
pub fn parse_ids(v: &[String], max: usize, what: &str) -> Result<Vec<ObjectId>> {
    if v.len() > max {
        return Err(protocol(format!(
            "{} {what} exceed the limit of {max}",
            v.len()
        )));
    }
    v.iter().map(|s| parse_id(s)).collect()
}

/// Turn an error response into an error.
pub fn into_error(resp: Response) -> ConcurvError {
    match resp {
        Response::Error {
            code,
            message,
            observed,
            seq,
            missing,
        } => {
            let code = ErrorCode::from_code_str(&code).unwrap_or(ErrorCode::Protocol);
            let mut m = format!("relay: {message}");
            if let Some(o) = observed {
                m.push_str(&format!(" (relay head {o}"));
                if let Some(s) = seq {
                    m.push_str(&format!(", seq {s}"));
                }
                m.push(')');
            }
            if !missing.is_empty() {
                m.push_str(&format!(" ({} objects missing)", missing.len()));
            }
            ConcurvError::new(code, m)
        }
        other => protocol(format!("unexpected response {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn messages_round_trip_and_unknown_fields_are_refused() {
        let req = Request::Publish {
            request_id: "ab".into(),
            branch: "main".into(),
            expected: None,
            proposed: "cd".into(),
            attestation: None,
        };
        let mut buf = Vec::new();
        write_message(&mut buf, &req).unwrap();
        assert_eq!(read_message::<Request>(&mut &buf[..]).unwrap(), req);

        let mut bad = Vec::new();
        write_message(&mut bad, &serde_json::json!({"type": "heads", "extra": 1})).unwrap();
        assert_eq!(
            read_message::<Request>(&mut &bad[..]).unwrap_err().code,
            ErrorCode::Protocol
        );
        let mut bad = Vec::new();
        write_message(&mut bad, &serde_json::json!({"type": "nope"})).unwrap();
        assert_eq!(
            read_message::<Request>(&mut &bad[..]).unwrap_err().code,
            ErrorCode::Protocol
        );
    }

    #[test]
    fn oversized_and_truncated_frames_are_refused() {
        let mut h = vec![FRAME_MESSAGE];
        h.extend_from_slice(&(MAX_MESSAGE + 1).to_be_bytes());
        assert_eq!(
            read_message::<Request>(&mut &h[..]).unwrap_err().code,
            ErrorCode::Protocol
        );
        let mut buf = Vec::new();
        write_message(&mut buf, &Request::Heads {}).unwrap();
        buf.truncate(buf.len() - 1);
        assert_eq!(
            read_message::<Request>(&mut &buf[..]).unwrap_err().code,
            ErrorCode::Network
        );
        let mut obj = vec![FRAME_OBJECT];
        obj.extend_from_slice(&5u64.to_be_bytes());
        assert_eq!(
            read_message::<Request>(&mut &obj[..]).unwrap_err().code,
            ErrorCode::Protocol
        );
    }
}
