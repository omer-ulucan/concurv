#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R3 gate, crash and restart: I/O failures injected at every write step
//! of a relay's publish and of a client's fetch, both as crashes (every
//! later I/O fails: the process died) and as transient errors (one write
//! fails, the process goes on). After a crash, reopening finds the old or
//! the new state, never a mix, and retrying completes the operation once.

mod common;

use concurv_core::{ObjectId, RandomId, SnapshotId};
use concurv_objects::{ALL_FAULT_POINTS, FaultMode, Faults, ObjectStore};
use concurv_sync::graph::objects_to_send;
use concurv_sync::relay::{PublishRequest, Session};
use concurv_sync::{Relay, RelayOptions, Scope, fetch, push};
use concurv_workspace::Repo;

use common::*;

/// A client repository with a published base and a new snapshot on top.
fn client_history() -> (tempfile::TempDir, Repo, SnapshotId, SnapshotId) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(d.path(), opts(Faults::none())).unwrap();
    std::fs::write(d.path().join("README"), "base\n").unwrap();
    let base = concurv_workspace::save(&r, "base")
        .unwrap()
        .snapshot
        .unwrap();
    std::fs::create_dir_all(d.path().join("src/deep")).unwrap();
    std::fs::write(d.path().join("src/a.rs"), "a\n").unwrap();
    std::fs::write(d.path().join("src/deep/b.rs"), "b\n".repeat(2000)).unwrap();
    let next = concurv_workspace::save(&r, "next")
        .unwrap()
        .snapshot
        .unwrap();
    (d, r, base, next)
}

fn upload(session: &Session, from: &ObjectStore, ids: &[ObjectId]) -> concurv_core::Result<()> {
    for id in ids {
        let (mut f, len) = from.open_raw(id)?;
        session.staging().receive_object(&mut f, len, u64::MAX)?;
    }
    Ok(())
}

/// A fresh relay at `dir` with `base` published; returns alice's token.
fn relay_with_base(dir: &std::path::Path, from: &ObjectStore, base: SnapshotId) -> String {
    Relay::init(dir).unwrap();
    let relay = Relay::open(dir, RelayOptions::default()).unwrap();
    let tok = relay
        .add_device("alice", &[Scope::Read, Scope::Write, Scope::Publish(None)])
        .unwrap();
    let dev = relay.authenticate(&tok).unwrap();
    let s = relay.session().unwrap();
    upload(&s, from, &objects_to_send(from, &[base], &[]).unwrap()).unwrap();
    relay
        .publish(
            &dev,
            Some(s.staging()),
            &PublishRequest {
                request_id: RandomId::generate().unwrap().to_hex(),
                branch: "main".into(),
                expected: None,
                proposed: base,
            },
        )
        .unwrap();
    tok
}

/// One publish of `next` on top of `base`, through the relay API.
fn publish_next(
    relay: &Relay,
    tok: &str,
    from: &ObjectStore,
    base: SnapshotId,
    next: SnapshotId,
    rid: &str,
) -> concurv_core::Result<bool> {
    let dev = relay.authenticate(tok)?;
    let s = relay.session()?;
    upload(&s, from, &objects_to_send(from, &[next], &[base])?)?;
    relay
        .publish(
            &dev,
            Some(s.staging()),
            &PublishRequest {
                request_id: rid.to_owned(),
                branch: "main".into(),
                expected: Some(base),
                proposed: next,
            },
        )
        .map(|p| p.replayed)
        .map_err(|e| e.error)
}

#[test]
fn relay_publish_survives_a_failure_at_every_write() {
    let (_c, client, base, next) = client_history();
    let store = client.store().clone();
    let outer = tempfile::tempdir().unwrap();
    // Count the write steps of one publish.
    let counting = outer.path().join("count");
    let tok = relay_with_base(&counting, &store, base);
    let rec = Faults::recording();
    let relay = Relay::open(
        &counting,
        RelayOptions {
            faults: rec.clone(),
            ..RelayOptions::default()
        },
    )
    .unwrap();
    publish_next(
        &relay,
        &tok,
        &store,
        base,
        next,
        &RandomId::generate().unwrap().to_hex(),
    )
    .unwrap();
    drop(relay);

    let (mut cases, mut committed_then_failed, mut rolled_back, mut untriggered) = (0, 0, 0, 0);
    for transient in [false, true] {
        for point in ALL_FAULT_POINTS {
            for nth in 1..=rec.hits(point) {
                cases += 1;
                let dir = outer.path().join(format!("r{cases}"));
                let tok = relay_with_base(&dir, &store, base);
                let faults = if transient {
                    Faults::fail_once_at(point, nth, FaultMode::TornWrite)
                } else {
                    Faults::fail_at(point, nth, FaultMode::TornWrite)
                };
                let rid = RandomId::generate().unwrap().to_hex();
                let relay = Relay::open(
                    &dir,
                    RelayOptions {
                        faults: faults.clone(),
                        ..RelayOptions::default()
                    },
                )
                .unwrap();
                let r = publish_next(&relay, &tok, &store, base, next, &rid);
                if !faults.triggered() {
                    // This run had fewer write steps than the counting run
                    // (random ids decide whether a fan-out directory is new).
                    r.unwrap();
                    untriggered += 1;
                    continue;
                }
                assert!(r.is_err(), "{point:?}#{nth}: the failure is reported");
                // Crash: a new process opens the relay. Transient: the same
                // process goes on.
                let relay = if transient {
                    relay
                } else {
                    drop(relay);
                    Relay::open(&dir, RelayOptions::default()).unwrap()
                };
                let h = relay.heads().unwrap()["main"];
                assert!(
                    (h.head, h.seq) == (base, 1) || (h.head, h.seq) == (next, 2),
                    "{point:?}#{nth} transient={transient}: old or new head"
                );
                relay_is_consistent(&dir);
                // Retrying with the same request id completes it exactly once.
                let replayed = publish_next(&relay, &tok, &store, base, next, &rid).unwrap();
                if h.seq == 2 {
                    committed_then_failed += 1;
                    assert!(replayed, "{point:?}#{nth}: already committed");
                } else {
                    rolled_back += 1;
                }
                let h = relay.heads().unwrap()["main"];
                assert_eq!((h.head, h.seq), (next, 2));
                relay_is_consistent(&dir);
            }
        }
    }
    eprintln!(
        "{cases} cases: {rolled_back} left the old head, {committed_then_failed} failed after the commit point, {untriggered} had no such write step"
    );
    assert!(cases > 20 && rolled_back > 0 && committed_then_failed > 0);
    assert!(untriggered * 10 < cases, "most cases must inject a failure");
}

#[test]
fn client_fetch_survives_a_failure_at_every_write() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    for i in 0..4 {
        write(&a, &format!("d/f{i}.txt"), &format!("{i}\n").repeat(100));
    }
    save_publish(&a, "one");
    write(&a, "d/f1.txt", "changed\n");
    save_publish(&a, "two");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let head = env.head("main").unwrap();

    // Count the write steps of one fetch.
    let probe = env.client("probe", &env.bob);
    let rec = Faults::recording();
    let probe = Repo::open(probe.root(), opts(rec.clone())).unwrap();
    fetch(&probe, "origin", &sync_opts()).unwrap();

    let (mut cases, mut fetched_anyway, mut untriggered) = (0, 0, 0);
    for transient in [false, true] {
        for point in ALL_FAULT_POINTS {
            for nth in 1..=rec.hits(point) {
                cases += 1;
                let c = env.client(&format!("c{cases}"), &env.bob);
                let faults = if transient {
                    Faults::fail_once_at(point, nth, FaultMode::TornWrite)
                } else {
                    Faults::fail_at(point, nth, FaultMode::TornWrite)
                };
                let faulty = Repo::open(c.root(), opts(faults.clone())).unwrap();
                let r = fetch(&faulty, "origin", &sync_opts());
                if !faults.triggered() {
                    // Fewer write steps than the counting run (random ids
                    // decide whether a fan-out directory is new).
                    r.unwrap();
                    untriggered += 1;
                    continue;
                }
                assert!(r.is_err(), "{point:?}#{nth}: the failure is reported");
                let c = if transient {
                    faulty
                } else {
                    drop(faulty);
                    Repo::open(c.root(), opts(Faults::none())).unwrap()
                };
                verify(&c);
                match local_branch(&c, "main") {
                    None => assert_eq!(tracking(&c, "main"), None, "{point:?}#{nth}"),
                    Some(h) => {
                        assert_eq!(h, head, "{point:?}#{nth}");
                        assert_eq!(tracking(&c, "main"), Some(head));
                        fetched_anyway += 1;
                    }
                }
                fetch(&c, "origin", &sync_opts()).unwrap();
                assert_eq!(local_branch(&c, "main"), Some(head));
                assert_eq!(tracking(&c, "main"), Some(head));
                verify(&c);
            }
        }
    }
    eprintln!(
        "{cases} fetch cases: {fetched_anyway} failed after the commit point, {untriggered} had no such write step"
    );
    assert!(cases > 20);
    assert!(untriggered * 10 < cases, "most cases must inject a failure");
}
