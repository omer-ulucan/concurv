#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R4 at the relay (RFC 0005 §7): the policy in the relay's current head
//! protects a branch; a push needs a passing attestation for exactly the
//! proposed head, vouched for by a device with the `attest` scope.

mod common;

use concurv_core::{ErrorCode, RandomId};
use concurv_sync::graph::objects_to_send;
use concurv_sync::{ConnectOptions, Connection, PushResult, RelayUrl, Scope, push};
use concurv_workspace::check::{CheckOptions, check};
use concurv_workspace::policy::POLICY_FILE;
use concurv_workspace::publish;

use common::*;

fn shell(unix: &str, win: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".into(), "/C".into(), win.into()]
    } else {
        vec!["sh".into(), "-c".into(), unix.into()]
    }
}

/// Passes unless `bad.flag` exists.
fn policy() -> String {
    serde_json::json!({
        "concurv_policy": 1,
        "protect": ["main"],
        "checks": [{"name": "no bad flag",
                    "argv": shell("test ! -f bad.flag", "if exist bad.flag exit 1"),
                    "timeout_s": 60}],
        "sandbox": "none",
    })
    .to_string()
}

fn trusted() -> CheckOptions {
    CheckOptions {
        trust: true,
        ..CheckOptions::default()
    }
}

#[test]
fn protected_branches_need_vouched_passing_attestations_at_the_relay() {
    let env = Env::new();
    let ci_token = env
        .relay()
        .add_device(
            "ci",
            &[
                Scope::Read,
                Scope::Write,
                Scope::Publish(None),
                Scope::Attest,
            ],
        )
        .unwrap();
    // The policy reaches the relay before any protection applies.
    let a = env.clone("a", &ci_token);
    write(&a, POLICY_FILE, &policy());
    save_publish(&a, "policy");
    push(&a, "origin", None, &sync_opts()).unwrap();
    let b = env.clone("b", &env.alice);

    // A (scope attest) checks, publishes locally and pushes with its
    // attestation.
    write(&a, "good.txt", "a");
    concurv_workspace::save(&a, "a1").unwrap();
    let ca = check(&a, &trusted()).unwrap();
    assert!(ca.passed);
    publish(&a).unwrap();
    let p = push(&a, "origin", None, &sync_opts()).unwrap();
    assert_eq!(p.result, PushResult::Pushed);
    let a1 = p.head;

    // B (no attest scope) has a passing attestation too, but the relay
    // only accepts attestations vouched for by `attest` devices.
    concurv_sync::fetch(&b, "origin", &sync_opts()).unwrap();
    concurv_workspace::update(&b).unwrap();
    write(&b, "good2.txt", "b");
    concurv_workspace::save(&b, "b1").unwrap();
    assert!(check(&b, &trusted()).unwrap().passed);
    publish(&b).unwrap();
    let e = push(&b, "origin", None, &sync_opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied, "{e}");
    assert_eq!(env.head("main"), Some(a1));

    // A failing candidate cannot even be pushed by an attest device: no
    // passing attestation exists for it.
    write(&a, "bad.flag", "x");
    let bad = concurv_workspace::save(&a, "bad")
        .unwrap()
        .snapshot
        .unwrap();
    let cb = check(&a, &CheckOptions::default()).unwrap();
    assert!(!cb.passed);
    // Local publish is refused too; the relay is tested directly below.
    assert_eq!(publish(&a).unwrap_err().code, ErrorCode::PolicyFail);

    // The relay itself refuses, whatever the client does.
    let url = RelayUrl::parse(&env.url).unwrap();
    let mut c = Connection::open(
        &url,
        &env.fingerprint,
        &ci_token,
        &ConnectOptions::default(),
    )
    .unwrap();
    let ids = objects_to_send(a.store(), &[bad], &[a1]).unwrap();
    let mut upload = ids.clone();
    upload.push(cb.attestation.object_id());
    upload.push(ca.attestation.object_id());
    c.upload(a.store(), &upload).unwrap();
    let rid = || RandomId::generate().unwrap().to_hex();
    // No attestation.
    let e = c.publish(&rid(), "main", Some(a1), bad, None).unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail, "{e}");
    // Not vouched for.
    let e = c
        .publish(&rid(), "main", Some(a1), bad, Some(cb.attestation))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("vouched"), "{e}");
    // Vouched, but failing.
    c.attest(cb.attestation).unwrap();
    let e = c
        .publish(&rid(), "main", Some(a1), bad, Some(cb.attestation))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("checks failed"), "{e}");
    // Vouched and passing, but for another candidate.
    c.attest(ca.attestation).unwrap();
    let e = c
        .publish(&rid(), "main", Some(a1), bad, Some(ca.attestation))
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("not the candidate"), "{e}");
    // A device without the scope cannot vouch.
    let mut cb_conn = Connection::open(
        &url,
        &env.fingerprint,
        &env.alice,
        &ConnectOptions::default(),
    )
    .unwrap();
    assert_eq!(
        cb_conn.attest(ca.attestation).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(env.head("main"), Some(a1));
    assert_eq!(env.heads()["main"].seq, 2);
    relay_is_consistent(&env.relay_dir);
}
