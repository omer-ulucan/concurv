#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R4 gate (RFC 0005): candidate-hash, policy and environment-bound
//! attestations; stale attestations refused; a failing merged test blocks
//! publication; checks see the candidate snapshot, not the working copy;
//! untrusted policies never run; sandbox and policy errors fail closed.

mod common;

use std::fs;
use std::path::Path;

use common::*;
use concurv_core::{AttestationId, ErrorCode};
use concurv_objects::encoding::{EXIT_NOT_STARTED, EXIT_TIMEOUT};
use concurv_objects::{Attestation, ObjectKind};
use concurv_workspace::check::{CheckOptions, attestations_for, check};
use concurv_workspace::policy::POLICY_FILE;
use concurv_workspace::{Repo, UpdateKind, publish, save, update, workspaces};

/// A check command running `unix` with `sh -c` or `win` with `cmd /C`.
fn shell(unix: &str, win: &str) -> Vec<String> {
    if cfg!(windows) {
        vec!["cmd".into(), "/C".into(), win.into()]
    } else {
        vec!["sh".into(), "-c".into(), unix.into()]
    }
}

fn policy(checks: &[(&str, Vec<String>, u64)], extra: serde_json::Value) -> String {
    let mut p = serde_json::json!({
        "concurv_policy": 1,
        "protect": ["main"],
        "checks": checks.iter().map(|(n, a, t)| serde_json::json!({
            "name": n, "argv": a, "timeout_s": t
        })).collect::<Vec<_>>(),
        "sandbox": "none",
    });
    if let serde_json::Value::Object(m) = extra {
        for (k, v) in m {
            p[k] = v;
        }
    }
    serde_json::to_string_pretty(&p).unwrap()
}

/// The merged tree fails if both flags exist: each side alone passes.
fn combined_check() -> Vec<String> {
    shell(
        "test ! -f a.flag -o ! -f b.flag",
        "if exist a.flag if exist b.flag exit 1",
    )
}

fn write(root: &Path, p: &str, c: &str) {
    let f = root.join(p);
    fs::create_dir_all(f.parent().unwrap()).unwrap();
    fs::write(f, c).unwrap();
}

/// A repository whose `main` holds `policy_text` (published before any
/// protection applied).
fn protected_repo(policy_text: &str) -> (tempfile::TempDir, Repo) {
    let (d, r) = new_repo();
    write(d.path(), POLICY_FILE, policy_text);
    write(d.path(), "README", "base\n");
    save(&r, "base with policy").unwrap();
    publish(&r).unwrap();
    (d, r)
}

fn trusted() -> CheckOptions {
    CheckOptions {
        trust: true,
        ..CheckOptions::default()
    }
}

#[test]
fn a_failing_merged_test_blocks_publication() {
    let (d, a) = protected_repo(&policy(
        &[("combined", combined_check(), 60)],
        serde_json::json!({}),
    ));
    let outer = tempfile::tempdir().unwrap();
    let bdir = outer.path().join("b");
    workspaces::add(&a, &bdir, Some("b")).unwrap();
    let b = Repo::open(&bdir, opts()).unwrap();

    // Without an attestation, publishing to protected main is refused.
    write(d.path(), "a.flag", "a");
    save(&a, "feature a").unwrap();
    assert_eq!(publish(&a).unwrap_err().code, ErrorCode::PolicyFail);
    // A passes its checks and publishes.
    let ca = check(&a, &trusted()).unwrap();
    assert!(ca.passed, "{:?}", ca.record);
    let p = publish(&a).unwrap();
    assert_eq!(p.attestation, Some(ca.attestation));

    // B is green on its own...
    write(&bdir, "b.flag", "b");
    save(&b, "feature b").unwrap();
    let cb = check(&b, &CheckOptions::default()).unwrap();
    assert!(cb.passed, "B alone passes");
    // ...but main moved: integrate, and the merged candidate fails.
    assert_eq!(update(&b).unwrap().kind, UpdateKind::Merged);
    let merged = b.head_snapshot().unwrap().unwrap();
    let e = publish(&b).unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail, "{e}");
    let cm = check(&b, &CheckOptions::default()).unwrap();
    assert!(!cm.passed);
    assert_eq!(cm.record.candidate, merged);
    let e = publish(&b).unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("checks failed: combined"), "{e}");
    // B's green attestation (for its unmerged head) never authorizes the merge.
    assert_eq!(
        attestations_for(&b, merged).unwrap(),
        vec![cm.attestation],
        "attestations are indexed by candidate"
    );
    let main = a.oplog().head().unwrap().1.view.branches["main"];
    assert_eq!(main, p.to, "main is unchanged");
    verify_ok(&a);
}

#[test]
fn stale_attestations_are_refused() {
    let ok = shell("true", "exit 0");
    let (d, r) = protected_repo(&policy(&[("ok", ok.clone(), 60)], serde_json::json!({})));
    write(d.path(), "x", "1");
    save(&r, "c1").unwrap();
    let c1 = check(&r, &trusted()).unwrap();
    assert!(c1.passed);
    // Another candidate: the attestation of c1 does not cover it.
    write(d.path(), "x", "2");
    save(&r, "c2").unwrap();
    let e = publish(&r).unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("no attestation is recorded"), "{e}");
    check(&r, &CheckOptions::default()).unwrap();
    publish(&r).unwrap();

    // The published head changes the policy: attestations made under the
    // old policy are stale for later candidates.
    write(
        d.path(),
        POLICY_FILE,
        &policy(
            &[("ok", ok.clone(), 60), ("again", ok, 60)],
            serde_json::json!({}),
        ),
    );
    save(&r, "stricter policy").unwrap();
    check(&r, &CheckOptions::default()).unwrap();
    publish(&r).unwrap();
    write(d.path(), "x", "3");
    save(&r, "c3").unwrap();
    // Untrusted: the new policy version needs its own approval.
    assert_eq!(
        check(&r, &CheckOptions::default()).unwrap_err().code,
        ErrorCode::UntrustedPolicy
    );
    let c3 = check(&r, &trusted()).unwrap();
    assert!(c3.newly_trusted && c3.passed);
    assert_eq!(c3.record.checks.len(), 2);
    publish(&r).unwrap();

    // A forged attestation that claims a pass the exit codes contradict
    // cannot even be read; one for another branch does not qualify.
    write(d.path(), "x", "4");
    let c4 = save(&r, "c4").unwrap().snapshot.unwrap();
    let mut forged = c3.record.clone();
    forged.candidate = c4;
    forged.branch = "other".into();
    let other = r.store().write_attestation(&forged).unwrap();
    let mut bytes = forged.encode().unwrap();
    forged.checks[0].exit_code = 1;
    bytes.truncate(bytes.len() - 1);
    bytes.push(1); // "pass" although a check failed
    let raw = r
        .store()
        .write_object(ObjectKind::Attestation, &{
            let mut b = forged.encode().unwrap();
            b.truncate(b.len() - 1);
            b.push(1);
            b
        })
        .unwrap();
    assert_eq!(
        r.store()
            .read_attestation(&AttestationId::from_object_id(raw))
            .unwrap_err()
            .code,
        ErrorCode::ObjectCorrupt
    );
    let idx = r.store_dir().join("attestations").join(c4.to_hex());
    fs::create_dir_all(idx.parent().unwrap()).unwrap();
    fs::write(&idx, format!("{}\n{}\n", other.to_hex(), raw.to_hex())).unwrap();
    let e = publish(&r).unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("made for branch other"), "{e}");
    assert!(e.message.contains("disagrees"), "{e}");
    verify_ok(&r);
}

#[test]
fn a_candidate_cannot_weaken_its_own_policy() {
    let fail = shell("exit 1", "exit 1");
    let (d, r) = protected_repo(&policy(
        &[("always fails", fail, 60)],
        serde_json::json!({}),
    ));
    // The candidate removes the protection from the policy file...
    write(
        d.path(),
        POLICY_FILE,
        &policy(
            &[("ok", shell("true", "exit 0"), 60)],
            serde_json::json!({"protect": []}),
        ),
    );
    save(&r, "disable checks").unwrap();
    // ...but the head's policy governs: still protected, and its check fails.
    assert_eq!(publish(&r).unwrap_err().code, ErrorCode::PolicyFail);
    let c = check(&r, &trusted()).unwrap();
    assert!(!c.passed);
    assert_eq!(c.record.checks[0].name, "always fails");
    assert_eq!(publish(&r).unwrap_err().code, ErrorCode::PolicyFail);
}

#[test]
fn checks_see_the_candidate_not_the_working_copy() {
    let need = shell("test -f ok.txt", "if not exist ok.txt exit 1");
    let (d, r) = protected_repo(&policy(
        &[("needs ok.txt", need, 60)],
        serde_json::json!({}),
    ));
    // ok.txt exists in the working copy but is ignored: not in the snapshot.
    write(d.path(), ".concurv/exclude", "ok.txt\n");
    write(d.path(), "ok.txt", "present only on disk");
    write(d.path(), "code", "v1");
    save(&r, "without ok.txt").unwrap();
    let c = check(&r, &trusted()).unwrap();
    assert!(!c.passed, "the check ran on the snapshot");
    // Once ok.txt is part of the candidate, it passes.
    fs::remove_file(d.path().join(".concurv/exclude")).unwrap();
    save(&r, "with ok.txt").unwrap();
    let c = check(&r, &CheckOptions::default()).unwrap();
    assert!(c.passed);
    // The working copy changes after the check: the attestation covers the
    // saved candidate only, and publish publishes exactly that.
    write(d.path(), "code", "edited after the check");
    let p = publish(&r).unwrap();
    assert_eq!(p.to, c.record.candidate);
    // The output of each check is kept.
    let out = r.store().read_blob(&c.record.checks[0].output).unwrap();
    assert!(out.len() < 10_000);
}

#[test]
fn untrusted_policies_never_run() {
    let outer = tempfile::tempdir().unwrap();
    let marker = outer.path().join("ran.txt");
    let m = marker.to_str().unwrap();
    // (cmd /C mishandles escaped quotes; temp paths here have no spaces.)
    let side = shell(&format!("echo ran >> '{m}'"), &format!("echo ran >> {m}"));
    let (d, r) = protected_repo(&policy(&[("side effect", side, 60)], serde_json::json!({})));
    write(d.path(), "y", "1");
    save(&r, "y").unwrap();
    let e = check(&r, &CheckOptions::default()).unwrap_err();
    assert_eq!(e.code, ErrorCode::UntrustedPolicy);
    assert!(e.message.contains("side effect"), "lists the commands: {e}");
    assert!(!marker.exists(), "nothing ran");
    assert_eq!(r.format_version().unwrap(), 1, "nothing written");
    let c = check(&r, &trusted()).unwrap();
    assert!(c.passed && marker.exists());
    assert_eq!(r.format_version().unwrap(), 3, "attestations need format 3");
}

#[test]
fn timeouts_missing_programs_and_bad_policies_fail_closed() {
    let slow = shell("sleep 5", "ping -n 6 127.0.0.1 > nul");
    let missing = vec!["concurv-no-such-program-xyz".to_owned()];
    let (d, r) = protected_repo(&policy(
        &[("slow", slow, 1), ("missing", missing, 60)],
        serde_json::json!({}),
    ));
    write(d.path(), "z", "1");
    save(&r, "z").unwrap();
    let c = check(&r, &trusted()).unwrap();
    assert!(!c.passed);
    assert_eq!(c.record.checks[0].exit_code, EXIT_TIMEOUT);
    assert!(
        c.record.checks[0].duration_ms < 4_000,
        "stopped at the timeout"
    );
    assert_eq!(c.record.checks[1].exit_code, EXIT_NOT_STARTED);

    // An unknown sandbox in the head's policy: protected, unsatisfiable.
    let (d, r) = protected_repo(&policy(
        &[("ok", shell("true", "exit 0"), 60)],
        serde_json::json!({"sandbox": "docker"}),
    ));
    write(d.path(), "z", "1");
    save(&r, "z").unwrap();
    let e = publish(&r).unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("sandbox"), "{e}");
    assert_eq!(
        check(&r, &trusted()).unwrap_err().code,
        ErrorCode::PolicyFail
    );

    // A policy limited to another OS cannot be satisfied here.
    let (d, r) = protected_repo(&policy(
        &[("ok", shell("true", "exit 0"), 60)],
        serde_json::json!({"environment": {"os": ["plan9"]}}),
    ));
    write(d.path(), "z", "1");
    save(&r, "z").unwrap();
    let e = check(&r, &trusted()).unwrap_err();
    assert_eq!(e.code, ErrorCode::PolicyFail);
    assert!(e.message.contains("plan9"), "{e}");
    // Unprotected branches are not affected by any of this.
    let (_d2, plain) = new_repo();
    fs::write(plain.root().join("f"), "x").unwrap();
    save(&plain, "plain").unwrap();
    publish(&plain).unwrap();
}

/// Attestations are bound to the environment they were made in.
#[test]
fn attestations_record_their_environment() {
    let (d, r) = protected_repo(&policy(
        &[("ok", shell("true", "exit 0"), 60)],
        serde_json::json!({"environment": {"os": [std::env::consts::OS]}}),
    ));
    write(d.path(), "e", "1");
    save(&r, "e").unwrap();
    let c = check(&r, &trusted()).unwrap();
    assert!(c.passed);
    let env: serde_json::Value = serde_json::from_str(&c.record.environment).unwrap();
    assert_eq!(env["os"], std::env::consts::OS);
    assert_eq!(env["arch"], std::env::consts::ARCH);
    // The same record with another OS would not qualify.
    let mut elsewhere: Attestation = c.record.clone();
    elsewhere.environment = serde_json::json!({"os": "plan9"}).to_string();
    let (_, pol) = concurv_workspace::policy::load(
        r.store(),
        r.oplog().head().unwrap().1.view.branches["main"],
    )
    .unwrap()
    .unwrap();
    let why = concurv_workspace::policy::satisfies(
        &elsewhere,
        c.record.candidate,
        "main",
        c.record.policy,
        &pol,
    )
    .unwrap_err();
    assert!(why.contains("plan9"), "{why}");
    publish(&r).unwrap();
}

fn verify_ok(r: &Repo) {
    let rep = concurv_workspace::verify(r, true);
    assert!(rep.problems.is_empty(), "{:?}", rep.problems);
}
