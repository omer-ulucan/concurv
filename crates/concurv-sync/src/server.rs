//! The relay's network side: a TCP listener, one thread per connection,
//! TLS 1.3, and the request loop of protocol version 1 (RFC 0003 §3).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use concurv_core::{ConcurvError, ErrorCode, Result};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::proto::{
    self, HeadInfo, MAX_QUERY_IDS, MAX_UPLOAD_BATCH, OBJECT_FORMAT, PROTOCOL, Request, Response,
    VERSION, network, parse_id, parse_ids, parse_snapshot, read_message, read_object,
    write_message, write_object,
};
use crate::relay::{Device, PublishRequest, Relay, Session};
use crate::tls::server_config;

/// Limits of a relay server.
#[derive(Clone, Debug)]
pub struct ServerOptions {
    /// Connections served at once; more are closed immediately.
    pub max_connections: usize,
    /// A connection idle for this long (no complete frame) is closed.
    pub idle_timeout: Duration,
    /// Delay before answering a failed authentication (slows guessing).
    pub auth_failure_delay: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            max_connections: 64,
            idle_timeout: Duration::from_secs(120),
            auth_failure_delay: Duration::from_millis(250),
        }
    }
}

/// A bound relay server.
pub struct Server {
    listener: TcpListener,
    relay: Arc<Relay>,
    tls: Arc<ServerConfig>,
    opts: ServerOptions,
    stop: Arc<AtomicBool>,
}

/// A server running on a background thread (tests, embedding).
pub struct ServerHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ServerHandle {
    /// Address the server listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stop accepting connections and wait for the accept loop to end.
    /// Connections already being served finish on their own.
    pub fn shutdown(mut self) {
        self.stop_now();
    }

    fn stop_now(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_secs(1));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop_now();
    }
}

impl Server {
    /// Bind `addr` (e.g. `127.0.0.1:7461`; port 0 picks a free port).
    pub fn bind(relay: Relay, addr: &str, opts: ServerOptions) -> Result<Self> {
        let tls = server_config(&relay.identity()?)?;
        let listener = TcpListener::bind(addr).map_err(|e| {
            ConcurvError::new(ErrorCode::Network, format!("cannot listen on {addr}: {e}"))
        })?;
        Ok(Self {
            listener,
            relay: Arc::new(relay),
            tls,
            opts,
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The bound address.
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener
            .local_addr()
            .map_err(|e| network(&e, "listener"))
    }

    /// Serve until the process ends (or [`ServerHandle::shutdown`]).
    pub fn run(self) -> Result<()> {
        let active = Arc::new(AtomicUsize::new(0));
        for conn in self.listener.incoming() {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let Ok(tcp) = conn else { continue };
            if active.fetch_add(1, Ordering::SeqCst) >= self.opts.max_connections {
                active.fetch_sub(1, Ordering::SeqCst);
                drop(tcp);
                continue;
            }
            let relay = self.relay.clone();
            let tls = self.tls.clone();
            let opts = self.opts.clone();
            let active = active.clone();
            thread::spawn(move || {
                let _ = handle(&relay, tls, tcp, &opts);
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
        Ok(())
    }

    /// Run on a background thread.
    pub fn spawn(self) -> Result<ServerHandle> {
        let addr = self.local_addr()?;
        let stop = self.stop.clone();
        let thread = thread::spawn(move || {
            let _ = self.run();
        });
        Ok(ServerHandle {
            addr,
            stop,
            thread: Some(thread),
        })
    }
}

fn handle(
    relay: &Relay,
    tls: Arc<ServerConfig>,
    tcp: TcpStream,
    opts: &ServerOptions,
) -> Result<()> {
    let set = |r: std::io::Result<()>| r.map_err(|e| network(&e, "socket"));
    set(tcp.set_read_timeout(Some(opts.idle_timeout)))?;
    set(tcp.set_write_timeout(Some(opts.idle_timeout)))?;
    set(tcp.set_nodelay(true))?;
    let conn =
        ServerConnection::new(tls).map_err(|e| ConcurvError::internal(format!("TLS: {e}")))?;
    let mut s = StreamOwned::new(conn, tcp);
    while s.conn.is_handshaking() {
        s.conn
            .complete_io(&mut s.sock)
            .map_err(|e| network(&e, "TLS handshake"))?;
    }
    let r = serve(relay, &mut s, opts);
    s.conn.send_close_notify();
    let _ = s.flush();
    r
}

fn send_error(s: &mut impl Write, e: &ConcurvError) -> Result<()> {
    write_message(s, &Response::error(e))
}

/// Serve one authenticated session on an established stream.
pub fn serve<S: Read + Write>(relay: &Relay, s: &mut S, opts: &ServerOptions) -> Result<()> {
    let device = match read_message::<Request>(s) {
        Ok(Request::Hello {
            protocol,
            versions,
            object_format,
            token,
        }) => {
            if protocol != PROTOCOL
                || !versions.contains(&VERSION)
                || object_format != OBJECT_FORMAT
            {
                let e = ConcurvError::new(
                    ErrorCode::ProtocolMismatch,
                    format!(
                        "this relay speaks {PROTOCOL} version {VERSION} with object format {OBJECT_FORMAT}"
                    ),
                );
                send_error(s, &e)?;
                return Err(e);
            }
            match relay.authenticate(&token) {
                Ok(d) => d,
                Err(e) => {
                    thread::sleep(opts.auth_failure_delay);
                    send_error(s, &e)?;
                    return Err(e);
                }
            }
        }
        Ok(_) => {
            let e = proto::protocol("the first message must be hello");
            send_error(s, &e)?;
            return Err(e);
        }
        Err(e) => {
            if e.code == ErrorCode::Protocol {
                send_error(s, &e)?;
            }
            return Err(e);
        }
    };
    write_message(
        s,
        &Response::Welcome {
            version: VERSION,
            relay: relay.relay_id()?,
            device: device.name.clone(),
            scopes: device.scopes.iter().map(|s| s.to_string()).collect(),
            max_blob: relay.options().max_blob,
        },
    )?;
    let mut session: Option<Session> = None;
    loop {
        let req = match read_message::<Request>(s) {
            Ok(r) => r,
            Err(e) => {
                if e.code == ErrorCode::Protocol {
                    send_error(s, &e)?;
                }
                return Err(e);
            }
        };
        match request(relay, &device, &mut session, s, req) {
            Ok(true) => {}
            Ok(false) => return Ok(()),
            Err(Fatal(e)) => {
                if e.code != ErrorCode::Network {
                    let _ = send_error(s, &e);
                }
                return Err(e);
            }
        }
    }
}

/// An error after which the stream cannot be trusted to be in sync.
struct Fatal(ConcurvError);

impl From<ConcurvError> for Fatal {
    fn from(e: ConcurvError) -> Self {
        Fatal(e)
    }
}

/// Handle one request. `Ok(false)` ends the session. Errors that leave the
/// stream in sync are answered and the session continues.
fn request<S: Read + Write>(
    relay: &Relay,
    device: &Device,
    session: &mut Option<Session>,
    s: &mut S,
    req: Request,
) -> std::result::Result<bool, Fatal> {
    let answer = |s: &mut S, r: Result<Response>| -> std::result::Result<bool, Fatal> {
        let resp = match r {
            Ok(resp) => resp,
            Err(e) if e.code == ErrorCode::Protocol => return Err(Fatal(e)),
            Err(e) => Response::error(&e),
        };
        write_message(s, &resp)?;
        Ok(true)
    };
    match req {
        Request::Hello { .. } => Err(Fatal(proto::protocol("hello was already sent"))),
        Request::Bye {} => Ok(false),
        Request::Heads {} => {
            let r = device.require_read().and_then(|()| {
                Ok(Response::Heads {
                    branches: relay
                        .heads()?
                        .into_iter()
                        .map(|(b, h)| {
                            (
                                b,
                                HeadInfo {
                                    head: h.head.to_hex(),
                                    seq: h.seq,
                                },
                            )
                        })
                        .collect(),
                })
            });
            answer(s, r)
        }
        Request::Missing { ids } => {
            let r = device.require_read().and_then(|()| {
                let ids = parse_ids(&ids, MAX_QUERY_IDS, "ids")?;
                Ok(Response::Missing {
                    ids: relay.missing(&ids).iter().map(|i| i.to_hex()).collect(),
                })
            });
            answer(s, r)
        }
        Request::Fetch { wants, haves } => {
            let ids = device.require_read().and_then(|()| {
                let parse = |v: &[String], max, what| -> Result<Vec<_>> {
                    parse_ids(v, max, what).map(|ids| {
                        ids.into_iter()
                            .map(concurv_core::SnapshotId::from_object_id)
                            .collect()
                    })
                };
                let wants = parse(&wants, proto::MAX_WANTS, "wants")?;
                let haves = parse(&haves, proto::MAX_HAVES, "haves")?;
                relay.objects_for(&wants, &haves)
            });
            match ids {
                Ok(ids) => {
                    let count = u32::try_from(ids.len()).map_err(|_| {
                        ConcurvError::new(ErrorCode::LimitExceeded, "pack too large")
                    })?;
                    write_message(s, &Response::Pack { count })?;
                    for id in &ids {
                        write_object(s, relay.store(), id)?;
                    }
                    s.flush().map_err(|e| network(&e, "send"))?;
                    Ok(true)
                }
                Err(e) => answer(s, Err(e)),
            }
        }
        Request::Upload { count } => {
            // Refusals come before `ready`, so no object frame is in flight
            // and the session stays usable.
            if let Err(e) = device.require_write() {
                return answer(s, Err(e));
            }
            if count > MAX_UPLOAD_BATCH {
                return Err(Fatal(proto::protocol(format!(
                    "upload of {count} objects exceeds the batch limit of {MAX_UPLOAD_BATCH}"
                ))));
            }
            if session.is_none() {
                *session = Some(relay.session()?);
            }
            let staging = session
                .as_ref()
                .map(|x| x.staging().clone())
                .ok_or_else(|| ConcurvError::internal("no session"))?;
            write_message(s, &Response::Ready {})?;
            // From here on, a bad object leaves unread frames behind: any
            // error ends the session.
            for _ in 0..count {
                read_object(s, &staging, relay.options().max_blob)?;
            }
            write_message(s, &Response::Stored { received: count })?;
            Ok(true)
        }
        Request::Intents {
            device: client,
            intents,
        } => {
            let r = relay
                .exchange_intents(device, &client, intents)
                .map(|intents| Response::Intents { intents });
            answer(s, r)
        }
        Request::Attest { id } => {
            let id = parse_id(&id)?;
            let r = relay
                .attest(
                    device,
                    session.as_ref().map(|x| x.staging()),
                    concurv_core::AttestationId::from_object_id(id),
                )
                .map(|()| Response::Attested { id: id.to_hex() });
            answer(s, r)
        }
        Request::Publish {
            request_id,
            branch,
            expected,
            proposed,
            attestation,
        } => {
            let parsed = (|| -> Result<PublishRequest> {
                Ok(PublishRequest {
                    request_id,
                    branch,
                    expected: expected.as_deref().map(parse_snapshot).transpose()?,
                    proposed: parse_snapshot(&proposed)?,
                    attestation: attestation
                        .as_deref()
                        .map(|a| parse_id(a).map(concurv_core::AttestationId::from_object_id))
                        .transpose()?,
                })
            })();
            let req = match parsed {
                Ok(r) => r,
                Err(e) => return Err(Fatal(e)),
            };
            let resp = match relay.publish(device, session.as_ref().map(|x| x.staging()), &req) {
                Ok(p) => Response::Published {
                    branch: p.branch,
                    head: p.head.to_hex(),
                    seq: p.seq,
                    replayed: p.replayed,
                },
                Err(pe) if pe.error.code == ErrorCode::Protocol => return Err(Fatal(pe.error)),
                Err(pe) => Response::Error {
                    code: pe.error.code.as_str().to_owned(),
                    message: pe.error.message,
                    observed: pe.observed.map(|h| h.head.to_hex()),
                    seq: pe.observed.map(|h| h.seq),
                    missing: pe.missing.iter().map(|m| m.to_hex()).collect(),
                },
            };
            write_message(s, &resp)?;
            Ok(true)
        }
    }
}
