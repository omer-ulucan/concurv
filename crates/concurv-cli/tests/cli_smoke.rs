#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! CLI smoke tests: run the real `concurv` binary through a complete
//! edit → status → diff → save → log → restore → verify sequence and check
//! observable filesystem results, JSON schema and exit codes (R1 gate item 10).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn cv(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_concurv"))
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("CONCURV_AUTHOR", "Smoke Test <smoke@example.com>")
        .output()
        .unwrap()
}

fn cv_ok(dir: &Path, args: &[&str]) -> String {
    let o = cv(dir, args);
    assert!(
        o.status.success(),
        "concurv {args:?} failed: {}\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

fn cv_json(dir: &Path, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    let o = cv(dir, &a);
    let v: Value = serde_json::from_slice(&o.stdout).unwrap_or_else(|e| {
        panic!(
            "concurv {args:?}: invalid JSON ({e}): {}",
            String::from_utf8_lossy(&o.stdout)
        )
    });
    assert_eq!(v["concurv_json_version"], 1);
    assert_eq!(v["ok"].as_bool().unwrap(), o.status.success());
    (o.status.code().unwrap(), v)
}

#[test]
fn full_cli_sequence() {
    let d = tempfile::tempdir().unwrap();
    let w = d.path();

    // init
    let out = cv_ok(w, &["init"]);
    assert!(out.contains("Initialized empty concurv repository"));
    let (code, v) = cv_json(w, &["init"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (11, Some("CONCURV_ALREADY_INITIALIZED"))
    );

    // edit → status
    fs::write(w.join("hello.txt"), "hello\nworld\n").unwrap();
    fs::create_dir(w.join("src")).unwrap();
    fs::write(w.join("src/data.bin"), [0u8, 159, 146, 150]).unwrap();
    let (code, v) = cv_json(w, &["status"]);
    assert_eq!(code, 0);
    assert_eq!(v["command"], "status");
    assert_eq!(v["data"]["clean"], false);
    assert_eq!(v["data"]["branch"], "main");
    assert!(v["data"]["head"].is_null());
    let paths: Vec<_> = v["data"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(paths, ["hello.txt", "src/data.bin"]);

    // save → log
    let (code, v) = cv_json(w, &["save", "-m", "first snapshot"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["created"], true);
    let s1 = v["data"]["snapshot"].as_str().unwrap().to_owned();
    assert!(cv_ok(w, &["status"]).contains("Working copy clean."));

    // edit → diff → save
    fs::write(w.join("hello.txt"), "hello\nconcurv\n").unwrap();
    fs::remove_file(w.join("src/data.bin")).unwrap();
    let diff = cv_ok(w, &["diff"]);
    assert!(
        diff.contains("--- a/hello.txt") && diff.contains("-world") && diff.contains("+concurv"),
        "{diff}"
    );
    assert!(
        diff.contains("deleted src/data.bin: binary files differ"),
        "{diff}"
    );
    let (_, v) = cv_json(w, &["diff"]);
    assert_eq!(v["data"]["files"].as_array().unwrap().len(), 2);
    let (_, v) = cv_json(w, &["save", "-m", "second"]);
    let s2 = v["data"]["snapshot"].as_str().unwrap().to_owned();
    let (_, v) = cv_json(w, &["save", "-m", "nothing"]);
    assert_eq!(v["data"]["created"], false);

    let (_, v) = cv_json(w, &["log"]);
    let snaps = v["data"]["snapshots"].as_array().unwrap();
    assert_eq!(snaps.len(), 2);
    assert_eq!(snaps[0]["snapshot"], s2.as_str());
    assert_eq!(snaps[0]["parents"][0], s1.as_str());
    assert_eq!(snaps[0]["author"], "Smoke Test <smoke@example.com>");
    let human_log = cv_ok(w, &["log"]);
    assert!(
        human_log.contains(&format!("snapshot {s2}")) && human_log.contains("    first snapshot")
    );

    // restore refuses a dirty working copy (exit 20) and changes nothing
    fs::write(w.join("hello.txt"), "unsaved work\n").unwrap();
    let (code, v) = cv_json(w, &["restore", "--snapshot", &s1[..12]]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (20, Some("CONCURV_DIRTY_WORKSPACE"))
    );
    assert_eq!(
        fs::read_to_string(w.join("hello.txt")).unwrap(),
        "unsaved work\n"
    );

    // --force keeps a backup and restores s1 exactly
    let (code, v) = cv_json(w, &["restore", "--snapshot", &s1[..12], "--force"]);
    assert_eq!(code, 0, "{v}");
    let backup = v["data"]["backup"].as_str().unwrap().to_owned();
    assert_eq!(
        fs::read_to_string(w.join("hello.txt")).unwrap(),
        "hello\nworld\n"
    );
    assert_eq!(
        fs::read(w.join("src/data.bin")).unwrap(),
        [0u8, 159, 146, 150]
    );
    // History did not move.
    let (_, v) = cv_json(w, &["status"]);
    assert_eq!(v["data"]["head"], s2.as_str());
    // Backup recovers the unsaved work.
    cv_ok(w, &["restore", "--snapshot", &backup, "--force"]);
    assert_eq!(
        fs::read_to_string(w.join("hello.txt")).unwrap(),
        "unsaved work\n"
    );

    // verify --full
    let (code, v) = cv_json(w, &["verify", "--full"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["data"]["problems"].as_array().unwrap().is_empty());
    assert!(cv_ok(w, &["verify"]).contains("Repository OK."));

    // corrupt an object: verify fails closed with exit 30 and reports it
    let obj = w.join(".concurv/objects").join(&s1[..2]).join(&s1[2..]);
    let mut bytes = fs::read(&obj).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x40;
    fs::write(&obj, &bytes).unwrap();
    let (code, v) = cv_json(w, &["verify", "--full"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (30, Some("CONCURV_OBJECT_CORRUPT")),
        "{v}"
    );
    assert!(
        v["data"]["problems"][0]["subject"]
            .as_str()
            .unwrap()
            .starts_with(&s1)
    );
    let (code, _) = cv_json(w, &["restore", "--snapshot", &s1, "--force"]);
    assert_eq!(code, 30);

    // missing object → exit 31
    fs::remove_file(&obj).unwrap();
    let (code, _) = cv_json(w, &["verify"]);
    assert_eq!(code, 31);
}

#[test]
fn errors_have_stable_codes() {
    let d = tempfile::tempdir().unwrap();
    let (code, v) = cv_json(d.path(), &["status"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (10, Some("CONCURV_NOT_A_REPO"))
    );
    let o = cv(d.path(), &["status"]);
    assert!(String::from_utf8_lossy(&o.stderr).contains("error[CONCURV_NOT_A_REPO]"));

    cv_ok(d.path(), &["init"]);
    let (code, v) = cv_json(d.path(), &["restore", "--snapshot", "xyz"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (2, Some("CONCURV_INVALID_ARGUMENT"))
    );
    let (code, v) = cv_json(d.path(), &["restore", "--snapshot", "0123456789abcdef"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (50, Some("CONCURV_NOT_FOUND"))
    );
    let (code, _) = cv_json(d.path(), &["save", "-m", ""]);
    assert_eq!(code, 2);

    // A non-snapshot object id is not accepted as a snapshot.
    fs::write(d.path().join("f"), "x").unwrap();
    let (_, v) = cv_json(d.path(), &["save", "-m", "s"]);
    let s = v["data"]["snapshot"].as_str().unwrap().to_owned();
    let (_, v) = cv_json(d.path(), &["log"]);
    let tree = v["data"]["snapshots"][0]["tree"]
        .as_str()
        .unwrap()
        .to_owned();
    let (code, v) = cv_json(d.path(), &["restore", "--snapshot", &tree]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (50, Some("CONCURV_NOT_FOUND"))
    );
    assert!(!s.is_empty());

    // Unknown subcommands are usage errors (clap, exit 2) — not advertised features.
    let o = cv(d.path(), &["sync"]);
    assert_eq!(o.status.code(), Some(2));
}
