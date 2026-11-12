#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R4 through the CLI: `check`, protected `publish`, `publish --check`, and
//! their exit codes and JSON.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn run(dir: &Path, args: &[&str]) -> (i32, Value) {
    let o = Command::new(env!("CARGO_BIN_EXE_concurv"))
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .env("CONCURV_AUTHOR", "verified-cli")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        serde_json::from_slice(&o.stdout).unwrap_or(Value::Null),
    )
}

fn check_cmd(script_unix: &str, script_win: &str) -> Value {
    if cfg!(windows) {
        serde_json::json!(["cmd", "/C", script_win])
    } else {
        serde_json::json!(["sh", "-c", script_unix])
    }
}

#[test]
fn protected_publish_through_the_cli() {
    let t = tempfile::tempdir().unwrap();
    let d = t.path();
    assert_eq!(run(d, &["init"]).0, 0);
    let policy = serde_json::json!({
        "concurv_policy": 1,
        "protect": ["main"],
        "checks": [{"name": "no bad flag",
                    "argv": check_cmd("test ! -f bad.flag", "if exist bad.flag exit 1"),
                    "timeout_s": 60}],
        "sandbox": "none",
    });
    fs::write(d.join("concurv-policy.json"), policy.to_string()).unwrap();
    assert_eq!(run(d, &["save", "-m", "policy"]).0, 0);
    assert_eq!(run(d, &["publish"]).0, 0, "no head yet: unprotected");

    fs::write(d.join("good.txt"), "ok").unwrap();
    assert_eq!(run(d, &["save", "-m", "good"]).0, 0);
    let (code, v) = run(d, &["publish"]);
    assert_eq!((code, v["error"]["code"].as_str()), (44, Some("CONCURV_POLICY_FAIL")));
    let (code, v) = run(d, &["check"]);
    assert_eq!((code, v["error"]["code"].as_str()), (45, Some("CONCURV_UNTRUSTED_POLICY")));
    let (code, v) = run(d, &["check", "--trust"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["passed"], true);
    assert_eq!(v["data"]["newly_trusted"], true);
    assert_eq!(v["data"]["checks"][0]["exit_code"], 0);
    let att = v["data"]["attestation"].as_str().unwrap().to_owned();
    let (code, v) = run(d, &["publish"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["attestation"], att);

    // A failing candidate: `publish --check` checks, fails, publishes nothing.
    fs::write(d.join("bad.flag"), "x").unwrap();
    assert_eq!(run(d, &["save", "-m", "bad"]).0, 0);
    let (code, v) = run(d, &["publish", "--check"]);
    assert_eq!((code, v["error"]["code"].as_str()), (44, Some("CONCURV_POLICY_FAIL")));
    // Fix it: `publish --check` checks and publishes.
    fs::remove_file(d.join("bad.flag")).unwrap();
    assert_eq!(run(d, &["save", "-m", "fixed"]).0, 0);
    let (code, v) = run(d, &["publish", "--check"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["data"]["attestation"].is_string());
    assert_eq!(run(d, &["verify", "--full"]).0, 0);
    assert_eq!(
        fs::read_to_string(d.join(".concurv/format")).unwrap(),
        "concurv-repo-format 3\n"
    );
}
