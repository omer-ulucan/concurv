//! Client side of protocol version 1: connecting to a relay and the
//! individual requests. The repository-level operations built on them are
//! in [`crate::sync`].

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::time::Duration;

use concurv_core::{ConcurvError, ErrorCode, ObjectId, Result, SnapshotId};
use concurv_objects::ObjectStore;
use rustls::{ClientConnection, StreamOwned};

use crate::proto::{
    self, DEFAULT_MAX_BLOB, DEFAULT_PORT, MAX_PACK, MAX_QUERY_IDS, MAX_UPLOAD_BATCH, OBJECT_FORMAT,
    PROTOCOL, Request, Response, VERSION, into_error, network, parse_id, parse_snapshot,
    read_message, read_object, write_message, write_object,
};
use crate::tls::{client_config, parse_fingerprint, server_name};

/// A relay address: `concurv://host[:port]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayUrl {
    /// Host name or IP address (IPv6 without brackets).
    pub host: String,
    /// TCP port.
    pub port: u16,
}

impl RelayUrl {
    /// Parse `concurv://host[:port]` (IPv6 hosts in brackets).
    pub fn parse(s: &str) -> Result<Self> {
        let bad = |why: &str| {
            ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!("invalid relay URL {s:?}: {why} (expected concurv://host[:port])"),
            )
        };
        let rest = s
            .strip_prefix("concurv://")
            .ok_or_else(|| bad("wrong scheme"))?;
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
            let (h, after) = v6.split_once(']').ok_or_else(|| bad("unclosed ["))?;
            match after.strip_prefix(':') {
                Some(p) => (h, Some(p)),
                None if after.is_empty() => (h, None),
                None => return Err(bad("unexpected text after ]")),
            }
        } else {
            match rest.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (rest, None),
            }
        };
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'_'))
        {
            return Err(bad("invalid host"));
        }
        let port = match port {
            None => DEFAULT_PORT,
            Some(p) => p
                .parse::<u16>()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| bad("invalid port"))?,
        };
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }
}

impl std::fmt::Display for RelayUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.host.contains(':') {
            write!(f, "concurv://[{}]:{}", self.host, self.port)
        } else {
            write!(f, "concurv://{}:{}", self.host, self.port)
        }
    }
}

/// Client-side options.
#[derive(Clone, Debug)]
pub struct ConnectOptions {
    /// Connect timeout, and the longest wait for any single read or write.
    pub timeout: Duration,
    /// Largest blob accepted from the relay.
    pub max_blob: u64,
    /// Test hook: the connection fails after this many bytes have been
    /// sent or received in total (simulated disconnect).
    pub fail_after_bytes: Option<u64>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(60),
            max_blob: DEFAULT_MAX_BLOB,
            fail_after_bytes: None,
        }
    }
}

trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

/// Wrapper that breaks the connection after a byte budget (tests).
struct Budget<S> {
    inner: S,
    left: Option<u64>,
}

impl<S> Budget<S> {
    fn spend(&mut self, n: usize) -> io::Result<usize> {
        match &mut self.left {
            None => Ok(n),
            Some(0) => Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "simulated disconnect",
            )),
            Some(l) => {
                let n = n.min(usize::try_from(*l).unwrap_or(usize::MAX));
                *l -= n as u64;
                Ok(n)
            }
        }
    }
}

impl<S: Read> Read for Budget<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.spend(buf.len())?;
        let got = self.inner.read(&mut buf[..n])?;
        if let Some(l) = &mut self.left {
            *l += (n - got) as u64;
        }
        Ok(got)
    }
}

impl<S: Write> Write for Budget<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.spend(buf.len())?;
        let put = self.inner.write(&buf[..n])?;
        if let Some(l) = &mut self.left {
            *l += (n - put) as u64;
        }
        Ok(put)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// What the relay said in `welcome`.
#[derive(Clone, Debug)]
pub struct Welcome {
    /// The relay's id.
    pub relay: String,
    /// This device's name at the relay.
    pub device: String,
    /// Its scopes.
    pub scopes: Vec<String>,
    /// Largest blob the relay accepts.
    pub max_blob: u64,
}

/// A relay's published head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteHeadInfo {
    /// Head.
    pub head: SnapshotId,
    /// Sequence number.
    pub seq: u64,
}

/// Result of a publish request.
#[derive(Clone, Debug)]
pub struct PublishReply {
    /// New head.
    pub head: SnapshotId,
    /// Its sequence number.
    pub seq: u64,
    /// The relay repeated an earlier result for this request id.
    pub replayed: bool,
}

/// An authenticated session with a relay.
pub struct Connection {
    stream: Box<dyn Stream>,
    welcome: Welcome,
    max_blob: u64,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("welcome", &self.welcome)
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Connect over TLS, check the relay's certificate against
    /// `fingerprint`, then authenticate with `token`. The token is sent
    /// only after the relay proved it holds the pinned certificate's key.
    pub fn open(
        url: &RelayUrl,
        fingerprint: &str,
        token: &str,
        opts: &ConnectOptions,
    ) -> Result<Self> {
        let pin = parse_fingerprint(fingerprint)?;
        let (cfg, mismatch) = client_config(&pin)?;
        let addrs: Vec<_> = (url.host.as_str(), url.port)
            .to_socket_addrs()
            .map_err(|e| network(&e, &format!("cannot resolve {url}")))?
            .collect();
        let mut last = None;
        let mut tcp = None;
        for a in &addrs {
            match TcpStream::connect_timeout(a, opts.timeout) {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let tcp = tcp.ok_or_else(|| match last {
            Some(e) => network(&e, &format!("cannot connect to {url}")),
            None => ConcurvError::new(ErrorCode::Network, format!("{url} has no address")),
        })?;
        let set = |r: io::Result<()>| r.map_err(|e| network(&e, "socket"));
        set(tcp.set_read_timeout(Some(opts.timeout)))?;
        set(tcp.set_write_timeout(Some(opts.timeout)))?;
        set(tcp.set_nodelay(true))?;
        let conn = ClientConnection::new(cfg, server_name()?)
            .map_err(|e| ConcurvError::internal(format!("TLS: {e}")))?;
        let mut s = StreamOwned::new(conn, tcp);
        while s.conn.is_handshaking() {
            if let Err(e) = s.conn.complete_io(&mut s.sock) {
                if mismatch.load(Ordering::SeqCst) {
                    return Err(ConcurvError::new(
                        ErrorCode::RelayIdentity,
                        format!(
                            "{url} presented a certificate that does not match the pinned fingerprint {fingerprint}; nothing was sent. If the relay's identity really changed, re-add the remote with the new fingerprint"
                        ),
                    ));
                }
                return Err(network(&e, &format!("TLS handshake with {url}")));
            }
        }
        let stream: Box<dyn Stream> = match opts.fail_after_bytes {
            None => Box::new(s),
            Some(n) => Box::new(Budget {
                inner: s,
                left: Some(n),
            }),
        };
        Self::start(stream, token, opts)
    }

    /// Run the protocol over an already established stream (tests).
    pub fn over<S: Read + Write + Send + 'static>(
        stream: S,
        token: &str,
        opts: &ConnectOptions,
    ) -> Result<Self> {
        Self::start(Box::new(stream), token, opts)
    }

    fn start(mut stream: Box<dyn Stream>, token: &str, opts: &ConnectOptions) -> Result<Self> {
        write_message(
            &mut stream,
            &Request::Hello {
                protocol: PROTOCOL.into(),
                versions: vec![VERSION],
                object_format: OBJECT_FORMAT,
                token: token.to_owned(),
            },
        )?;
        match read_message::<Response>(&mut stream)? {
            Response::Welcome {
                version,
                relay,
                device,
                scopes,
                max_blob,
            } if version == VERSION => Ok(Self {
                stream,
                welcome: Welcome {
                    relay,
                    device,
                    scopes,
                    max_blob,
                },
                max_blob: opts.max_blob,
            }),
            Response::Welcome { version, .. } => Err(ConcurvError::new(
                ErrorCode::ProtocolMismatch,
                format!("relay chose protocol version {version}, this build speaks {VERSION}"),
            )),
            other => Err(into_error(other)),
        }
    }

    /// The relay's welcome.
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    fn call(&mut self, req: &Request) -> Result<Response> {
        write_message(&mut self.stream, req)?;
        read_message(&mut self.stream)
    }

    /// Published heads.
    pub fn heads(&mut self) -> Result<BTreeMap<String, RemoteHeadInfo>> {
        match self.call(&Request::Heads {})? {
            Response::Heads { branches } => branches
                .into_iter()
                .map(|(b, h)| {
                    Ok((
                        b,
                        RemoteHeadInfo {
                            head: parse_snapshot(&h.head)?,
                            seq: h.seq,
                        },
                    ))
                })
                .collect(),
            other => Err(into_error(other)),
        }
    }

    /// Download the objects of `wants` (minus the history of `haves`) into
    /// `staging`. Returns the number of objects received. Every object is
    /// verified on receipt; completeness is checked by the caller.
    pub fn fetch_into(
        &mut self,
        wants: &[SnapshotId],
        haves: &[SnapshotId],
        staging: &ObjectStore,
    ) -> Result<u32> {
        let req = Request::Fetch {
            wants: wants.iter().map(|w| w.to_hex()).collect(),
            haves: haves.iter().map(|h| h.to_hex()).collect(),
        };
        match self.call(&req)? {
            Response::Pack { count } if count <= MAX_PACK => {
                for _ in 0..count {
                    read_object(&mut self.stream, staging, self.max_blob)?;
                }
                Ok(count)
            }
            Response::Pack { count } => Err(proto::protocol(format!(
                "relay announced {count} objects, more than the limit of {MAX_PACK}"
            ))),
            other => Err(into_error(other)),
        }
    }

    /// The subset of `ids` the relay lacks.
    pub fn missing(&mut self, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(MAX_QUERY_IDS) {
            let req = Request::Missing {
                ids: chunk.iter().map(|i| i.to_hex()).collect(),
            };
            match self.call(&req)? {
                Response::Missing { ids } => {
                    for i in ids {
                        let id = parse_id(&i)?;
                        if !chunk.contains(&id) {
                            return Err(proto::protocol(
                                "relay answered with an id it was not asked about",
                            ));
                        }
                        out.push(id);
                    }
                }
                other => return Err(into_error(other)),
            }
        }
        Ok(out)
    }

    /// Upload objects from `store` into this session's quarantine at the
    /// relay, in the given order.
    pub fn upload(&mut self, store: &ObjectStore, ids: &[ObjectId]) -> Result<()> {
        for chunk in ids.chunks(MAX_UPLOAD_BATCH as usize) {
            let count = u32::try_from(chunk.len()).unwrap_or(MAX_UPLOAD_BATCH);
            match self.call(&Request::Upload { count })? {
                Response::Ready {} => {}
                other => return Err(into_error(other)),
            }
            for id in chunk {
                write_object(&mut self.stream, store, id)?;
            }
            self.stream
                .flush()
                .map_err(|e| network(&e, "connection lost"))?;
            match read_message(&mut self.stream)? {
                Response::Stored { received } if received == count => {}
                other => return Err(into_error(other)),
            }
        }
        Ok(())
    }

    /// Vouch for attestation `id` (uploaded before, or already at the
    /// relay). Needs the `attest` scope.
    pub fn attest(&mut self, id: concurv_core::AttestationId) -> Result<()> {
        match self.call(&Request::Attest { id: id.to_hex() })? {
            Response::Attested { id: got } if got == id.to_hex() => Ok(()),
            other => Err(into_error(other)),
        }
    }

    /// Exchange advisory intents (RFC 0006 §2): replace this device's
    /// intents at the relay (if `submit`), and get every active intent.
    pub fn intents(
        &mut self,
        device: &str,
        submit: Option<Vec<concurv_workspace::coord::Intent>>,
    ) -> Result<Vec<concurv_workspace::coord::Intent>> {
        let req = Request::Intents {
            device: device.to_owned(),
            intents: submit,
        };
        match self.call(&req)? {
            Response::Intents { intents } => Ok(intents),
            other => Err(into_error(other)),
        }
    }

    /// Compare-and-swap of `branch` at the relay.
    pub fn publish(
        &mut self,
        request_id: &str,
        branch: &str,
        expected: Option<SnapshotId>,
        proposed: SnapshotId,
        attestation: Option<concurv_core::AttestationId>,
    ) -> Result<PublishReply> {
        let req = Request::Publish {
            request_id: request_id.to_owned(),
            branch: branch.to_owned(),
            expected: expected.map(|e| e.to_hex()),
            proposed: proposed.to_hex(),
            attestation: attestation.map(|a| a.to_hex()),
        };
        match self.call(&req)? {
            Response::Published {
                branch: b,
                head,
                seq,
                replayed,
            } => {
                let head = parse_snapshot(&head)?;
                if b != branch || head != proposed {
                    return Err(proto::protocol("relay confirmed a different publish"));
                }
                Ok(PublishReply {
                    head,
                    seq,
                    replayed,
                })
            }
            other => Err(into_error(other)),
        }
    }

    /// End the session politely.
    pub fn close(mut self) {
        let _ = write_message(&mut self.stream, &Request::Bye {});
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn relay_urls() {
        let u = RelayUrl::parse("concurv://example.org").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("example.org", DEFAULT_PORT));
        let u = RelayUrl::parse("concurv://10.0.0.2:9000/").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("10.0.0.2", 9000));
        let u = RelayUrl::parse("concurv://[::1]:7000").unwrap();
        assert_eq!((u.host.as_str(), u.port), ("::1", 7000));
        assert_eq!(u.to_string(), "concurv://[::1]:7000");
        for bad in [
            "http://x",
            "concurv://",
            "concurv://x:0",
            "concurv://x:99999",
            "concurv://x y",
            "concurv://[::1",
            "concurv://a/b",
        ] {
            assert!(RelayUrl::parse(bad).is_err(), "{bad}");
        }
    }
}
