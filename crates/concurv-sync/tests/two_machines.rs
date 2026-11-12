#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R3 gate (docs/ROADMAP_REVISED.md §2): two independent repositories save
//! offline and reconnect in both orders; every acknowledged snapshot ends up
//! on the relay; one authoritative head; stale publishes are explicit
//! conflicts; duplicate and reordered messages are harmless; disconnects at
//! any byte change nothing or complete; authentication and permissions are
//! enforced; a rewound relay is never followed.

mod common;

use std::collections::HashSet;
use std::thread;

use concurv_core::{ErrorCode, RandomId};
use concurv_sync::graph::objects_to_send;
use concurv_sync::{BranchAction, ConnectOptions, Connection, PushResult, RelayUrl, fetch, push};
use concurv_workspace::branches::{self, CreateOptions};
use concurv_workspace::{UpdateKind, UpdateOptions, merge_branch, publish, update};

use common::*;

fn branch_action(o: &concurv_sync::FetchOutcome, b: &str) -> BranchAction {
    o.branches
        .iter()
        .find(|r| r.branch == b)
        .unwrap_or_else(|| panic!("no report for {b}: {o:?}"))
        .action
}

#[test]
fn two_machines_save_offline_and_reconnect_in_both_orders() {
    for a_first in [true, false] {
        let env = Env::new();
        let a = env.clone("a", &env.alice);
        write(&a, "README", "base\n");
        let base = save_publish(&a, "base");
        assert_eq!(
            push(&a, "origin", None, &sync_opts()).unwrap().result,
            PushResult::Pushed
        );
        let b = env.clone("b", &env.bob);
        assert_eq!(read(&b, "README"), "base\n");
        assert_eq!(tracking(&b, "main"), Some(base));

        // Offline: each machine saves and publishes to its local main.
        write(&a, "a.txt", "from a\n");
        let a1 = save_publish(&a, "a1");
        write(&b, "b.txt", "from b\n");
        let b1 = save_publish(&b, "b1");

        let (first, second, f1, s1) = if a_first {
            (&a, &b, a1, b1)
        } else {
            (&b, &a, b1, a1)
        };
        // The first to reconnect publishes; the relay has one head.
        let p = push(first, "origin", None, &sync_opts()).unwrap();
        assert_eq!(
            (p.result, p.from, p.seq),
            (PushResult::Pushed, Some(base), Some(2))
        );
        assert_eq!(tracking(first, "main"), Some(f1));
        // The second is refused explicitly; nothing is published.
        let e = push(second, "origin", None, &sync_opts()).unwrap_err();
        assert_eq!(e.code, ErrorCode::Diverged, "{e}");
        assert_eq!(env.head("main"), Some(f1));

        // It fetches: the divergence is preserved, its local main stays.
        let f = fetch(second, "origin", &sync_opts()).unwrap();
        assert_eq!(branch_action(&f, "main"), BranchAction::Diverged);
        assert!(f.objects > 0);
        assert_eq!(local_branch(second, "main"), Some(s1));
        assert_eq!(tracking(second, "main"), Some(f1));
        // Pushing again is still refused: rule 3, no implicit publication.
        assert_eq!(
            push(second, "origin", None, &sync_opts()).unwrap_err().code,
            ErrorCode::Diverged
        );

        // Integrate in the workspace, publish locally, push.
        let m = merge_branch(second, "remote/origin/main", UpdateOptions::default()).unwrap();
        assert_eq!(m.kind, UpdateKind::Merged);
        publish(second).unwrap();
        let p = push(second, "origin", None, &sync_opts()).unwrap();
        assert_eq!(
            (p.result, p.from, p.seq),
            (PushResult::Pushed, Some(f1), Some(3))
        );
        relay_history_contains(&env, "main", &[base, a1, b1]);

        // The first catches up by fast-forward and sees both files.
        let f = fetch(first, "origin", &sync_opts()).unwrap();
        assert_eq!(branch_action(&f, "main"), BranchAction::FastForwarded);
        assert_eq!(update(first).unwrap().kind, UpdateKind::FastForward);
        for r in [&a, &b] {
            assert_eq!(read(r, "a.txt"), "from a\n");
            assert_eq!(read(r, "b.txt"), "from b\n");
            verify(r);
        }
        assert_eq!(local_branch(&a, "main"), local_branch(&b, "main"));
        assert_eq!(local_branch(&a, "main"), env.head("main"));
        relay_is_consistent(&env.relay_dir);
    }
}

#[test]
fn fetch_then_update_integrates_unpublished_saves() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "base\n");
    save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let b = env.clone("b", &env.bob);
    write(&a, "README", "base\nmore from a\n");
    let a1 = save_publish(&a, "a1");
    push(&a, "origin", None, &sync_opts()).unwrap();
    // B saved but did not publish: its local main is behind and simply
    // fast-forwards; `update` merges into the workspace.
    write(&b, "notes.txt", "b's notes\n");
    let b1 = concurv_workspace::save(&b, "b1").unwrap().snapshot.unwrap();
    let f = fetch(&b, "origin", &sync_opts()).unwrap();
    assert_eq!(branch_action(&f, "main"), BranchAction::FastForwarded);
    assert_eq!(local_branch(&b, "main"), Some(a1));
    assert_eq!(update(&b).unwrap().kind, UpdateKind::Merged);
    publish(&b).unwrap();
    assert_eq!(
        push(&b, "origin", None, &sync_opts()).unwrap().result,
        PushResult::Pushed
    );
    relay_history_contains(&env, "main", &[a1, b1]);
    assert_eq!(read(&b, "README"), "base\nmore from a\n");
    // Pushing again: nothing to do.
    let p = push(&b, "origin", None, &sync_opts()).unwrap();
    assert_eq!((p.result, p.objects), (PushResult::UpToDate, 0));
    // A has not seen B's merge: it cannot tell "behind" from "diverged",
    // so its push is refused until it fetches; then it is up to date.
    let e = push(&a, "origin", None, &sync_opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    assert!(e.message.contains("concurv fetch"), "{e}");
    let f = fetch(&a, "origin", &sync_opts()).unwrap();
    assert_eq!(branch_action(&f, "main"), BranchAction::FastForwarded);
    assert_eq!(
        push(&a, "origin", None, &sync_opts()).unwrap().result,
        PushResult::UpToDate
    );
}

#[test]
fn racing_pushes_have_one_winner_and_all_work_lands_eventually() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "base\n");
    save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let clients: Vec<_> = (0..5)
        .map(|i| {
            let c = env.clone(
                &format!("c{i}"),
                if i % 2 == 0 { &env.alice } else { &env.bob },
            );
            write(&c, &format!("f{i}.txt"), &format!("client {i}\n"));
            c
        })
        .collect();
    let mut acked = Vec::new();
    for (i, c) in clients.iter().enumerate() {
        acked.push(
            concurv_workspace::save(c, &format!("c{i}"))
                .unwrap()
                .snapshot
                .unwrap(),
        );
        publish(c).unwrap();
    }
    let results: Vec<_> = thread::scope(|s| {
        let hs: Vec<_> = clients
            .iter()
            .map(|c| s.spawn(move || push(c, "origin", None, &no_retry())))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winners = results
        .iter()
        .filter(|r| matches!(r, Ok(o) if o.result == PushResult::Pushed))
        .count();
    assert_eq!(winners, 1, "{results:?}");
    for r in &results {
        if let Err(e) = r {
            assert!(
                matches!(e.code, ErrorCode::StaleHead | ErrorCode::Diverged),
                "{e}"
            );
        }
    }
    // Every loser integrates and retries until its work is in.
    for c in &clients {
        for _ in 0..10 {
            match push(c, "origin", None, &sync_opts()) {
                Ok(o) if matches!(o.result, PushResult::Pushed | PushResult::UpToDate) => break,
                Ok(o) => panic!("unexpected {o:?}"),
                Err(e) => {
                    assert!(matches!(e.code, ErrorCode::StaleHead | ErrorCode::Diverged));
                    fetch(c, "origin", &sync_opts()).unwrap();
                    merge_branch(c, "remote/origin/main", UpdateOptions::default()).unwrap();
                    publish(c).unwrap();
                }
            }
        }
    }
    relay_history_contains(&env, "main", &acked);
    let head = env.heads()["main"];
    assert!(head.seq >= 6);
    relay_is_consistent(&env.relay_dir);
}

fn connect(env: &Env, token: &str) -> Connection {
    Connection::open(
        &RelayUrl::parse(&env.url).unwrap(),
        &env.fingerprint,
        token,
        &ConnectOptions::default(),
    )
    .unwrap()
}

#[test]
fn stale_publish_is_refused_and_retries_are_idempotent() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "base\n");
    let base = save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();
    write(&a, "x", "1");
    let s1 = save_publish(&a, "s1");
    write(&a, "x", "2");
    let s2 = concurv_workspace::save(&a, "s2").unwrap().snapshot.unwrap();

    let mut c1 = connect(&env, &env.alice);
    let mut c2 = connect(&env, &env.bob);
    assert_eq!(c1.heads().unwrap()["main"].head, base);
    assert_eq!(c2.heads().unwrap()["main"].head, base);
    // Duplicate and reordered uploads are harmless: send everything twice,
    // in reverse dependency order.
    let ids = objects_to_send(a.store(), &[s2], &[base]).unwrap();
    let mut twice: Vec<_> = ids.iter().rev().copied().collect();
    twice.extend(ids.iter().copied());
    c1.upload(a.store(), &twice).unwrap();
    c2.upload(a.store(), &ids).unwrap();
    let rid = RandomId::generate().unwrap().to_hex();
    let r = c1.publish(&rid, "main", Some(base), s1).unwrap();
    assert_eq!((r.seq, r.replayed), (2, false));
    // c2 observed `base` too: its compare-and-swap is stale.
    let e = c2
        .publish(
            &RandomId::generate().unwrap().to_hex(),
            "main",
            Some(base),
            s2,
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::StaleHead, "{e}");
    assert!(
        e.message.contains(&s1.to_hex()),
        "names the observed head: {e}"
    );
    assert_eq!(env.head("main"), Some(s1));
    // A retry of the first publish (its reply lost) returns the original
    // result and does not publish again.
    let mut c3 = connect(&env, &env.alice);
    let r = c3.publish(&rid, "main", Some(base), s1).unwrap();
    assert_eq!((r.seq, r.replayed), (2, true));
    assert_eq!(env.heads()["main"].seq, 2);
    // Reusing the request id for another publish is a protocol error.
    let e = c3.publish(&rid, "main", Some(s1), s2).unwrap_err();
    assert_eq!(e.code, ErrorCode::Protocol);
    // Heads only move forward, even with the right `expected`.
    let mut c4 = connect(&env, &env.alice);
    let e = c4
        .publish(
            &RandomId::generate().unwrap().to_hex(),
            "main",
            Some(s1),
            base,
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Diverged);
    // A publish whose graph the relay lacks is refused.
    let mut c5 = connect(&env, &env.alice);
    write(&a, "y", "unsent");
    let s3 = concurv_workspace::save(&a, "s3").unwrap().snapshot.unwrap();
    let e = c5
        .publish(
            &RandomId::generate().unwrap().to_hex(),
            "main",
            Some(s1),
            s3,
        )
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ObjectMissing);
    assert_eq!(env.head("main"), Some(s1));
    relay_is_consistent(&env.relay_dir);
}

#[test]
fn authentication_and_permissions_are_enforced() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "base\n");
    save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let url = RelayUrl::parse(&env.url).unwrap();
    let open =
        |tok: &str| Connection::open(&url, &env.fingerprint, tok, &ConnectOptions::default());
    // Unknown tokens, malformed tokens.
    let fake = format!("cvt1_{}", "0".repeat(64));
    for t in [fake.as_str(), "nope", ""] {
        assert_eq!(open(t).unwrap_err().code, ErrorCode::AuthFailed, "{t:?}");
    }
    // A read-only device can fetch but not push.
    let r = env.client("r", &env.reader);
    fetch(&r, "origin", &sync_opts()).unwrap();
    update(&r).unwrap();
    write(&r, "r.txt", "reader\n");
    save_publish(&r, "r1");
    assert_eq!(
        push(&r, "origin", None, &sync_opts()).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    // `publish:main` cannot create other branches.
    let m = env.clone("m", &env.main_only);
    write(&m, "m.txt", "m\n");
    concurv_workspace::save(&m, "m1").unwrap();
    branches::create(&m, "feature", CreateOptions::default()).unwrap();
    assert_eq!(
        push(&m, "origin", Some("feature"), &sync_opts())
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    publish(&m).unwrap();
    assert_eq!(
        push(&m, "origin", None, &sync_opts()).unwrap().result,
        PushResult::Pushed
    );
    // Revocation takes effect at the next connection.
    env.relay().revoke_device("bob").unwrap();
    assert_eq!(open(&env.bob).unwrap_err().code, ErrorCode::AuthFailed);
    assert_eq!(env.heads().len(), 1, "only main was published");
}

#[test]
fn a_rewound_relay_is_reported_and_never_followed() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "v1\n");
    let v1 = save_publish(&a, "v1");
    push(&a, "origin", None, &sync_opts()).unwrap();
    write(&a, "README", "v2\n");
    let v2 = save_publish(&a, "v2");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let b = env.clone("b", &env.bob);
    assert_eq!(tracking(&b, "main"), Some(v2));
    // The relay is restored from an old backup: main is back at v1.
    let state_path = env.relay_dir.join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["branches"]["main"] = serde_json::json!({"head": v1.to_hex(), "seq": 1});
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let f = fetch(&b, "origin", &sync_opts()).unwrap();
    assert_eq!(branch_action(&f, "main"), BranchAction::Rewound);
    assert!(f.operation.is_none(), "no reference changed");
    assert_eq!(local_branch(&b, "main"), Some(v2));
    assert_eq!(tracking(&b, "main"), Some(v2));
    // Pushing to it is refused too: nothing would be lost silently, but the
    // situation needs a human.
    assert_eq!(
        push(&b, "origin", None, &sync_opts()).unwrap_err().code,
        ErrorCode::Diverged
    );
    assert_eq!(env.head("main"), Some(v1));
}

/// Simulated disconnects at every point of a push: the push either
/// completes or fails with `CONCURV_NETWORK`; the relay then holds the old
/// or the new head, never anything else; a plain retry finishes it, and no
/// publish is ever applied twice. Covers lost replies (the relay applied
/// the publish, the client never heard).
#[test]
fn disconnects_during_push_lose_nothing_and_never_publish_twice() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "base\n");
    let base = save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();
    // Each cut starts from this relay state, with a fresh clone whose work
    // has the same size, so every push sends the same number of bytes.
    let state_path = env.relay_dir.join("state.json");
    let original = std::fs::read(&state_path).unwrap();
    let mut k = 0usize;
    let mut cut = |n: Option<u64>, retries: u32| {
        k += 1;
        std::fs::write(&state_path, &original).unwrap();
        let c = env.clone(&format!("c{k}"), &env.alice);
        for i in 0..20 {
            write(
                &c,
                &format!("d/f{i}.txt"),
                &format!("cut {k:06} file {i}\n").repeat(50),
            );
        }
        let snap = save_publish(&c, "work");
        let opts = concurv_sync::SyncOptions {
            connect: ConnectOptions {
                fail_after_bytes: n,
                ..sync_opts().connect
            },
            retries,
        };
        let r = push(&c, "origin", None, &opts);
        (c, snap, r)
    };
    let (mut lo, mut hi) = (0u64, 1 << 22);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if cut(Some(mid), 0).2.is_ok() {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let total = lo;
    let mut budgets: Vec<u64> = (0..10).map(|j| total * j / 10).collect();
    budgets.extend(total.saturating_sub(40)..total);
    let (mut lost_replies, mut replayed) = (0, 0);
    for (i, n) in budgets.into_iter().enumerate() {
        let (c, snap, r) = cut(Some(n), (i % 2) as u32);
        match r {
            Ok(o) => {
                assert_eq!(o.result, PushResult::Pushed);
                replayed += usize::from(o.replayed);
            }
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Network, "budget {n}: {e}");
                let h = env.heads()["main"];
                assert!(
                    (h.head, h.seq) == (base, 1) || (h.head, h.seq) == (snap, 2),
                    "budget {n}: old or new head only"
                );
                lost_replies += usize::from(h.seq == 2);
                let o = push(&c, "origin", None, &sync_opts()).unwrap();
                assert!(matches!(
                    o.result,
                    PushResult::Pushed | PushResult::UpToDate
                ));
            }
        }
        let h = env.heads()["main"];
        assert_eq!(
            (h.head, h.seq),
            (snap, 2),
            "budget {n}: exactly one publish"
        );
        assert_eq!(tracking(&c, "main"), Some(snap));
        relay_is_consistent(&env.relay_dir);
        verify(&c);
    }
    eprintln!("push size {total} bytes; lost replies {lost_replies}; replayed {replayed}");
    assert!(lost_replies > 0, "some cut must lose the publish reply");
    assert!(
        replayed > 0,
        "some retry must be answered from the request memory"
    );
}

/// Simulated disconnects at every point of a fetch: nothing is adopted and
/// no reference changes unless the fetch completes; a retry completes it.
#[test]
fn disconnects_during_fetch_change_nothing() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    for round in 0..3 {
        for i in 0..10 {
            write(
                &a,
                &format!("d{round}/f{i}.txt"),
                &format!("{round}/{i}\n").repeat(40),
            );
        }
        save_publish(&a, &format!("round {round}"));
    }
    push(&a, "origin", None, &sync_opts()).unwrap();
    let head = env.head("main").unwrap();
    let with_budget = |n: u64| concurv_sync::SyncOptions {
        connect: ConnectOptions {
            fail_after_bytes: Some(n),
            ..sync_opts().connect
        },
        retries: 0,
    };
    let mut total = None;
    let mut seen = HashSet::new();
    let mut n = 0u64;
    let mut idx = 0;
    while total.is_none() {
        idx += 1;
        let b = env.client(&format!("b{idx}"), &env.bob);
        let (op_before, _) = b.oplog().head().unwrap();
        let objs = object_count(&b);
        match fetch(&b, "origin", &with_budget(n)) {
            Ok(o) => {
                total = Some(n);
                assert_eq!(local_branch(&b, "main"), Some(head));
                assert!(o.operation.is_some());
            }
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Network, "budget {n}: {e}");
                assert_eq!(b.oplog().head().unwrap().0, op_before, "budget {n}");
                assert_eq!(object_count(&b), objs, "budget {n}: nothing adopted");
                seen.insert(e.message.clone());
                fetch(&b, "origin", &sync_opts()).unwrap();
                assert_eq!(local_branch(&b, "main"), Some(head));
            }
        }
        verify(&b);
        n = if n < 2048 { n + 97 } else { n * 2 };
    }
    eprintln!("fetch completed with a budget of {total:?} bytes after {idx} cuts");
    assert!(idx > 5);
}

/// Adding a remote upgrades the repository to format 2 (RFC 0001 §7):
/// secondary workspaces keep opening, fetch and update work from them, and
/// undo after a fetch leaves the remote-tracking references alone.
#[test]
fn format_upgrade_keeps_secondary_workspaces_working() {
    let env = Env::new();
    let a = env.clone("a", &env.alice);
    write(&a, "README", "base\n");
    save_publish(&a, "base");
    push(&a, "origin", None, &sync_opts()).unwrap();

    let root = env.dir.path().join("plain");
    std::fs::create_dir_all(&root).unwrap();
    let p = concurv_workspace::Repo::init(&root, opts(concurv_objects::Faults::none())).unwrap();
    assert!(!p.supports_remotes().unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join(".concurv/format")).unwrap(),
        "concurv-repo-format 1\n"
    );
    let second = env.dir.path().join("second");
    concurv_workspace::workspaces::add(&p, &second, None).unwrap();
    concurv_sync::remote::add(&p, "origin", &env.url, &env.fingerprint, &env.bob).unwrap();
    assert!(p.supports_remotes().unwrap());
    assert_eq!(
        std::fs::read_to_string(root.join(".concurv/format")).unwrap(),
        "concurv-repo-format 2\n"
    );
    // The secondary workspace opens and syncs.
    let s = concurv_workspace::Repo::open(&second, opts(concurv_objects::Faults::none())).unwrap();
    write(&s, "local.txt", "secondary\n");
    let saved = concurv_workspace::save(&s, "local")
        .unwrap()
        .snapshot
        .unwrap();
    let f = fetch(&s, "origin", &sync_opts()).unwrap();
    assert_eq!(branch_action(&f, "main"), BranchAction::Created);
    assert_eq!(update(&s).unwrap().kind, UpdateKind::Merged);
    assert_eq!(read(&s, "README"), "base\n");
    // Undo reverses the workspace's update only; the fetch stays.
    let tracked = tracking(&s, "main");
    concurv_workspace::undo(&s).unwrap();
    assert_eq!(s.head_snapshot().unwrap(), Some(saved));
    assert_eq!(tracking(&s, "main"), tracked);
    assert_eq!(local_branch(&p, "main"), tracked);
    // A second remote add is refused; listing shows the one remote.
    assert!(concurv_sync::remote::add(&p, "origin", &env.url, &env.fingerprint, &env.bob).is_err());
    assert_eq!(concurv_sync::remote::list(&s).unwrap().len(), 1);
    verify(&p);
    verify(&s);
}
