#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R3 gate, hostile peers (threat model T9, T10): malformed messages and
//! objects sent to a relay, a relay presenting the wrong certificate, and a
//! malicious relay that lies about heads or sends bad objects. Nothing a
//! peer sends is trusted: refusals change no state on the receiving side.

mod common;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use concurv_core::{ErrorCode, SnapshotId};
use concurv_objects::encoding::encode_object;
use concurv_objects::{ObjectKind, ObjectStore};
use concurv_sync::proto::{
    HeadInfo, MAX_UPLOAD_BATCH, MAX_WANTS, OBJECT_FORMAT, PROTOCOL, Request, Response, VERSION,
    read_message, write_message,
};
use concurv_sync::tls::{
    Identity, client_config, generate_identity, parse_fingerprint, server_config, server_name,
};
use concurv_sync::{BranchAction, ConnectOptions, fetch, push};
use rustls::{ClientConnection, ServerConnection, StreamOwned};

use common::*;

type Client = StreamOwned<ClientConnection, TcpStream>;

fn raw(env: &Env) -> Client {
    let (cfg, _) = client_config(&parse_fingerprint(&env.fingerprint).unwrap()).unwrap();
    let tcp = TcpStream::connect(env.server.as_ref().unwrap().addr()).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut s = StreamOwned::new(
        ClientConnection::new(cfg, server_name().unwrap()).unwrap(),
        tcp,
    );
    while s.conn.is_handshaking() {
        s.conn.complete_io(&mut s.sock).unwrap();
    }
    s
}

fn hello(token: &str) -> Request {
    Request::Hello {
        protocol: PROTOCOL.into(),
        versions: vec![VERSION],
        object_format: OBJECT_FORMAT,
        token: token.into(),
    }
}

fn authed(env: &Env) -> Client {
    let mut s = raw(env);
    write_message(&mut s, &hello(&env.alice)).unwrap();
    assert!(matches!(
        read_message::<Response>(&mut s).unwrap(),
        Response::Welcome { .. }
    ));
    s
}

fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![kind];
    v.extend_from_slice(&(body.len() as u64).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn send(s: &mut Client, bytes: &[u8]) {
    s.write_all(bytes).unwrap();
    s.flush().unwrap();
}

/// The next message is an error with `code`, and then the relay hangs up.
fn refused_and_closed(s: &mut Client, code: ErrorCode) {
    match read_message::<Response>(s) {
        Ok(Response::Error { code: c, .. }) => assert_eq!(c, code.as_str()),
        other => panic!("expected {code}, got {other:?}"),
    }
    let mut buf = [0u8; 1];
    assert!(
        matches!(s.read(&mut buf), Ok(0) | Err(_)),
        "connection must be closed"
    );
}

fn refused(s: &mut Client, code: ErrorCode) {
    match read_message::<Response>(s) {
        Ok(Response::Error { code: c, .. }) => assert_eq!(c, code.as_str()),
        other => panic!("expected {code}, got {other:?}"),
    }
}

#[test]
fn malformed_traffic_is_refused_and_changes_nothing() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "base\n");
    save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let before = env.heads();
    let op = |s: &str| serde_json::to_vec(&serde_json::json!({"type": s})).unwrap();

    // Before hello: any other request, garbage JSON, oversized or unknown frames.
    let mut s = raw(&env);
    send(&mut s, &frame(1, &op("heads")));
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = raw(&env);
    send(&mut s, &frame(1, b"{not json"));
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = raw(&env);
    let mut big = vec![1u8];
    big.extend_from_slice(&(2u64 << 20).to_be_bytes());
    send(&mut s, &big);
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = raw(&env);
    send(&mut s, &frame(9, b"?"));
    refused_and_closed(&mut s, ErrorCode::Protocol);
    // No common version.
    let mut s = raw(&env);
    let mut h = hello(&env.alice);
    if let Request::Hello { versions, .. } = &mut h {
        *versions = vec![VERSION + 1];
    }
    write_message(&mut s, &h).unwrap();
    refused_and_closed(&mut s, ErrorCode::ProtocolMismatch);

    // After hello: a second hello, an object frame instead of a request.
    let mut s = authed(&env);
    write_message(&mut s, &hello(&env.alice)).unwrap();
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = authed(&env);
    send(
        &mut s,
        &frame(2, &encode_object(ObjectKind::Blob, b"x").unwrap()),
    );
    refused_and_closed(&mut s, ErrorCode::Protocol);

    // Uploads of bad objects: corrupt, wrong kind, truncated header, a
    // message where an object belongs, too many objects.
    let upload = |s: &mut Client, body: Vec<u8>| {
        write_message(s, &Request::Upload { count: 1 }).unwrap();
        assert_eq!(read_message::<Response>(s).unwrap(), Response::Ready {});
        send(s, &body);
    };
    let mut blob = encode_object(ObjectKind::Blob, b"payload").unwrap();
    blob[0] = b'Z';
    let mut s = authed(&env);
    upload(&mut s, frame(2, &blob));
    refused_and_closed(&mut s, ErrorCode::ObjectCorrupt);
    let mut s = authed(&env);
    upload(
        &mut s,
        frame(2, &encode_object(ObjectKind::Operation, b"op").unwrap()),
    );
    refused_and_closed(&mut s, ErrorCode::WrongObjectKind);
    let mut s = authed(&env);
    upload(&mut s, frame(2, b"CNV1"));
    refused_and_closed(&mut s, ErrorCode::ObjectCorrupt);
    let mut s = authed(&env);
    upload(&mut s, frame(1, &op("heads")));
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = authed(&env);
    write_message(
        &mut s,
        &Request::Upload {
            count: MAX_UPLOAD_BATCH + 1,
        },
    )
    .unwrap();
    refused_and_closed(&mut s, ErrorCode::Protocol);

    // Bad publishes: malformed ids end the session; a well-formed publish
    // of an unknown snapshot is answered and the session goes on.
    let mut s = authed(&env);
    write_message(
        &mut s,
        &Request::Publish {
            request_id: "zz".into(),
            branch: "main".into(),
            expected: None,
            proposed: "00".repeat(32),
        },
    )
    .unwrap();
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = authed(&env);
    write_message(
        &mut s,
        &Request::Publish {
            request_id: "ab".repeat(16),
            branch: "main".into(),
            expected: None,
            proposed: "not hex".into(),
        },
    )
    .unwrap();
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = authed(&env);
    write_message(
        &mut s,
        &Request::Publish {
            request_id: "ab".repeat(16),
            branch: "evil".into(),
            expected: None,
            proposed: "11".repeat(32),
        },
    )
    .unwrap();
    refused(&mut s, ErrorCode::ObjectMissing);
    // Bad branch names are refused before anything else.
    for (i, name) in ["HEAD", "a..b", "remote/x/y", "-x", "a b"]
        .into_iter()
        .enumerate()
    {
        write_message(
            &mut s,
            &Request::Publish {
                request_id: format!("{i:032x}"),
                branch: name.into(),
                expected: None,
                proposed: before["main"].head.to_hex(),
            },
        )
        .unwrap();
        match read_message::<Response>(&mut s).unwrap() {
            Response::Error { code, .. } => assert!(
                code == ErrorCode::InvalidArgument.as_str() || code == ErrorCode::Protocol.as_str(),
                "{name}: {code}"
            ),
            other => panic!("{name}: {other:?}"),
        }
    }

    // Fetch: too many wants ends the session; an unknown want does not.
    let mut s = authed(&env);
    write_message(
        &mut s,
        &Request::Fetch {
            wants: vec!["00".repeat(32); MAX_WANTS + 1],
            haves: Vec::new(),
        },
    )
    .unwrap();
    refused_and_closed(&mut s, ErrorCode::Protocol);
    let mut s = authed(&env);
    write_message(
        &mut s,
        &Request::Fetch {
            wants: vec!["22".repeat(32)],
            haves: Vec::new(),
        },
    )
    .unwrap();
    refused(&mut s, ErrorCode::NotFound);
    write_message(&mut s, &Request::Heads {}).unwrap();
    assert!(matches!(
        read_message::<Response>(&mut s).unwrap(),
        Response::Heads { .. }
    ));

    // A client that disappears mid-object.
    let mut s = authed(&env);
    let blob = encode_object(ObjectKind::Blob, &[7u8; 100_000]).unwrap();
    write_message(&mut s, &Request::Upload { count: 1 }).unwrap();
    assert_eq!(
        read_message::<Response>(&mut s).unwrap(),
        Response::Ready {}
    );
    send(&mut s, &frame(2, &blob)[..50_000]);
    drop(s);

    // Nothing changed, the relay still serves, and no session debris stays.
    assert_eq!(env.heads(), before);
    relay_is_consistent(&env.relay_dir);
    let b = env.clone("b", &env.bob);
    assert_eq!(read(&b, "README"), "base\n");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let left = std::fs::read_dir(env.relay_dir.join("tmp"))
            .unwrap()
            .count();
        if left == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{left} temp entries left in the relay"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// A TLS server with `id` that runs `script` on its first connection.
fn fake_relay<F>(id: &Identity, script: F) -> (SocketAddr, JoinHandle<()>)
where
    F: FnOnce(&mut StreamOwned<ServerConnection, TcpStream>) + Send + 'static,
{
    let cfg = server_config(id).unwrap();
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let h = thread::spawn(move || {
        let (tcp, _) = l.accept().unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut s = StreamOwned::new(ServerConnection::new(cfg).unwrap(), tcp);
        script(&mut s);
    });
    (addr, h)
}

#[test]
fn a_wrong_certificate_stops_the_client_before_the_token_is_sent() {
    let env = Env::new();
    let impostor = generate_identity().unwrap();
    let got = Arc::new(AtomicUsize::new(0));
    let g = got.clone();
    let (addr, h) = fake_relay(&impostor, move |s| {
        let mut buf = [0u8; 4096];
        while let Ok(n) = s.read(&mut buf) {
            if n == 0 {
                break;
            }
            g.fetch_add(n, Ordering::SeqCst);
        }
    });
    let c = env.client("c", &env.alice);
    concurv_sync::remote::remove(&c, "origin").unwrap();
    concurv_sync::remote::add(
        &c,
        "origin",
        &format!("concurv://{addr}"),
        &env.fingerprint,
        &env.alice,
    )
    .unwrap();
    let e = fetch(&c, "origin", &sync_opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::RelayIdentity, "{e}");
    h.join().unwrap();
    assert_eq!(
        got.load(Ordering::SeqCst),
        0,
        "no application data reached the impostor"
    );
}

/// Objects of a small history built in a scratch repository: raw bytes by
/// id, and the head snapshot.
fn history() -> (tempfile::TempDir, ObjectStore, SnapshotId) {
    let d = tempfile::tempdir().unwrap();
    let r = concurv_workspace::Repo::init(d.path(), opts(concurv_objects::Faults::none())).unwrap();
    std::fs::create_dir_all(d.path().join("src")).unwrap();
    std::fs::write(d.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    std::fs::write(d.path().join("README"), "hello\n").unwrap();
    let s = concurv_workspace::save(&r, "one")
        .unwrap()
        .snapshot
        .unwrap();
    (d, r.store().clone(), s)
}

fn raw_object(store: &ObjectStore, id: &concurv_core::ObjectId) -> Vec<u8> {
    let (mut f, _) = store.open_raw(id).unwrap();
    let mut v = Vec::new();
    f.read_to_end(&mut v).unwrap();
    v
}

/// A malicious relay: advertises `heads`, answers one fetch with `pack`
/// (raw object encodings; `announce` objects are announced).
fn lying_relay(
    id: &Identity,
    heads: BTreeMap<String, HeadInfo>,
    pack: Vec<Vec<u8>>,
    announce: u32,
) -> (SocketAddr, JoinHandle<()>) {
    fake_relay(id, move |s| {
        let _: Request = read_message(s).unwrap();
        write_message(
            s,
            &Response::Welcome {
                version: VERSION,
                relay: "ff".repeat(16),
                device: "x".into(),
                scopes: vec!["read".into()],
                max_blob: 1 << 30,
            },
        )
        .unwrap();
        let _: Request = read_message(s).unwrap();
        write_message(s, &Response::Heads { branches: heads }).unwrap();
        if let Ok(Request::Fetch { .. }) = read_message::<Request>(s) {
            write_message(s, &Response::Pack { count: announce }).unwrap();
            for o in &pack {
                let _ = s.write_all(&frame(2, o));
            }
            let _ = s.flush();
        }
        s.conn.send_close_notify();
        let _ = s.flush();
    })
}

fn victim(env: &Env, name: &str, addr: SocketAddr, id: &Identity) -> concurv_workspace::Repo {
    let c = env.client(name, &env.alice);
    concurv_sync::remote::remove(&c, "origin").unwrap();
    concurv_sync::remote::add(
        &c,
        "origin",
        &format!("concurv://{addr}"),
        &concurv_sync::tls::fingerprint(&id.cert),
        &env.alice,
    )
    .unwrap();
    c
}

#[test]
fn a_lying_relay_cannot_inject_objects_or_references() {
    let env = Env::new();
    let id = generate_identity().unwrap();
    let (_d, store, head) = history();
    let snap = store.read_snapshot(&head).unwrap();
    let tree = store.read_tree(&snap.tree).unwrap();
    let main = |h: SnapshotId| {
        let mut m = BTreeMap::new();
        m.insert(
            "main".to_owned(),
            HeadInfo {
                head: h.to_hex(),
                seq: 1,
            },
        );
        m
    };
    let everything: Vec<Vec<u8>> = concurv_sync::graph::objects_to_send(&store, &[head], &[])
        .unwrap()
        .iter()
        .map(|i| raw_object(&store, i))
        .collect();
    let without_tree: Vec<Vec<u8>> = concurv_sync::graph::objects_to_send(&store, &[head], &[])
        .unwrap()
        .iter()
        .filter(|i| **i != snap.tree.object_id())
        .map(|i| raw_object(&store, i))
        .collect();
    let other_snapshot = {
        let mut s2 = snap.clone();
        s2.message = "forged".into();
        encode_object(ObjectKind::Snapshot, &s2.encode().unwrap()).unwrap()
    };
    let cases: Vec<(&str, Vec<Vec<u8>>, u32, ErrorCode)> = vec![
        // Content that hashes to something else than the advertised head.
        ("forged", vec![other_snapshot], 1, ErrorCode::ObjectMissing),
        // An incomplete history.
        (
            "incomplete",
            without_tree.clone(),
            without_tree.len() as u32,
            ErrorCode::ObjectMissing,
        ),
        // Objects that never travel.
        (
            "operation",
            vec![encode_object(ObjectKind::Operation, b"x").unwrap()],
            1,
            ErrorCode::WrongObjectKind,
        ),
        // Announcing more than it sends, then hanging up.
        (
            "short",
            everything.clone(),
            everything.len() as u32 + 3,
            ErrorCode::Network,
        ),
    ];
    for (i, (what, pack, announce, code)) in cases.into_iter().enumerate() {
        let (addr, h) = lying_relay(&id, main(head), pack, announce);
        let c = victim(&env, &format!("v{i}"), addr, &id);
        let (op_before, _) = c.oplog().head().unwrap();
        let objects = object_count(&c);
        let e = fetch(&c, "origin", &sync_opts()).unwrap_err();
        assert_eq!(e.code, code, "{what}: {e}");
        assert_eq!(
            c.oplog().head().unwrap().0,
            op_before,
            "{what}: no reference changed"
        );
        assert_eq!(object_count(&c), objects, "{what}: nothing adopted");
        verify(&c);
        h.join().unwrap();
    }

    // A snapshot whose tree id names a blob: a kind mismatch, refused while
    // checking the graph.
    let blob_id = tree.entries().iter().find(|e| e.kind.is_blob()).unwrap().id;
    let mut s2 = snap.clone();
    s2.tree = concurv_core::TreeId::from_object_id(blob_id);
    let bad_snap = s2.encode().unwrap();
    let bad_id = SnapshotId::from_object_id(concurv_objects::encoding::hash_object(
        ObjectKind::Snapshot,
        &bad_snap,
    ));
    let pack = vec![
        raw_object(&store, &blob_id),
        encode_object(ObjectKind::Snapshot, &bad_snap).unwrap(),
    ];
    let (addr, h) = lying_relay(&id, main(bad_id), pack, 2);
    let c = victim(&env, "kind", addr, &id);
    let objects = object_count(&c);
    assert_eq!(
        fetch(&c, "origin", &sync_opts()).unwrap_err().code,
        ErrorCode::WrongObjectKind
    );
    assert_eq!(object_count(&c), objects);
    h.join().unwrap();

    // Invalid branch names are ignored; names that collide with each other
    // (only a misbehaving relay has both) never create aliasing local
    // references: in name order, the first is created, the rest reported.
    let mut heads = main(head);
    for name in [
        "HEAD",
        "a..b",
        "remote/x/y",
        "-x",
        "x.lock",
        "con",
        "topic",
        "Topic/x",
    ] {
        heads.insert(
            name.to_owned(),
            HeadInfo {
                head: head.to_hex(),
                seq: 1,
            },
        );
    }
    let (addr, h) = lying_relay(&id, heads, everything.clone(), everything.len() as u32);
    let c = victim(&env, "names", addr, &id);
    let f = fetch(&c, "origin", &sync_opts()).unwrap();
    h.join().unwrap();
    for r in &f.branches {
        let expect = match r.branch.as_str() {
            "main" | "Topic/x" => BranchAction::Created,
            "topic" => BranchAction::NameCollision,
            _ => BranchAction::InvalidName,
        };
        assert_eq!(r.action, expect, "{}", r.branch);
    }
    let view = c.oplog().head().unwrap().1.view;
    assert_eq!(
        view.branches.keys().collect::<Vec<_>>(),
        vec!["Topic/x", "main"]
    );
    assert_eq!(
        view.remotes["origin"].keys().collect::<Vec<_>>(),
        vec!["Topic/x", "main", "topic"],
        "remote-tracking references keep what the relay said"
    );
    verify(&c);

    // A blob over the client's limit is refused.
    let (addr, h) = lying_relay(&id, main(head), everything.clone(), everything.len() as u32);
    let c = victim(&env, "limit", addr, &id);
    let small = concurv_sync::SyncOptions {
        connect: ConnectOptions {
            max_blob: 3,
            ..sync_opts().connect
        },
        retries: 0,
    };
    assert_eq!(
        fetch(&c, "origin", &small).unwrap_err().code,
        ErrorCode::LimitExceeded
    );
    h.join().unwrap();
}
