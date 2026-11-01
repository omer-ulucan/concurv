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

/// A backup made by `restore --force` can be found again through `op log`
/// even if the id printed by `restore` was lost.
#[test]
fn op_log_lists_operations_and_backups() {
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    cv_ok(w, &["init"]);
    fs::write(w.join("f.txt"), "v1\n").unwrap();
    let (_, v) = cv_json(w, &["save", "-m", "one"]);
    let s1 = v["data"]["snapshot"].as_str().unwrap().to_owned();
    fs::write(w.join("f.txt"), "v2\n").unwrap();
    let (_, v) = cv_json(w, &["save", "-m", "two"]);
    let s2 = v["data"]["snapshot"].as_str().unwrap().to_owned();
    fs::write(w.join("f.txt"), "unsaved work\n").unwrap();
    let (code, _) = cv_json(w, &["restore", "--snapshot", &s1, "--force"]);
    assert_eq!(code, 0);

    let (code, v) = cv_json(w, &["op", "log"]);
    assert_eq!(code, 0);
    assert_eq!(v["command"], "op log");
    let ops = v["data"]["operations"].as_array().unwrap();
    let kinds: Vec<_> = ops.iter().map(|o| o["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["restore", "save", "save", "init"]);
    // Restore does not move history: head after restore is still s2.
    assert_eq!(ops[0]["head"], s2.as_str());
    // R2: save moves only the workspace head; `main` moves on publish.
    assert!(ops[1]["branches"]["main"].is_null());
    assert!(ops[3]["head"].is_null() && ops[3]["workspace"].is_null());
    assert_eq!(ops[0]["parents"][0], ops[1]["operation"]);

    // The backup listed in the op log recovers the unsaved work.
    let backup = ops[0]["backup"].as_str().unwrap().to_owned();
    assert!(ops[1..].iter().all(|o| o["backup"].is_null()));
    cv_ok(w, &["restore", "--snapshot", &backup, "--force"]);
    assert_eq!(
        fs::read_to_string(w.join("f.txt")).unwrap(),
        "unsaved work\n"
    );

    // Limits, and human output mentions the backup and how to recover it.
    let (_, v) = cv_json(w, &["op", "log", "-n", "2"]);
    assert_eq!(v["data"]["operations"].as_array().unwrap().len(), 2);
    let (_, v) = cv_json(w, &["op", "log", "-n", "0"]);
    assert!(v["data"]["operations"].as_array().unwrap().is_empty());
    let human = cv_ok(w, &["op", "log"]);
    assert!(
        human.contains(&format!("backup of discarded working copy: {backup}")),
        "{human}"
    );
    assert!(human.contains("(init)") && human.contains("initialize repository"));

    // op log is read-only.
    let before = fs::read(w.join(".concurv/op_head")).unwrap();
    cv_ok(w, &["op", "log"]);
    assert_eq!(fs::read(w.join(".concurv/op_head")).unwrap(), before);
}

/// R2 commands: workspace add/list, publish, update; diverged (42) and
/// conflict (43) exit codes; JSON contract.
#[test]
fn workspaces_publish_and_update() {
    let outer = tempfile::tempdir().unwrap();
    let a = outer.path().join("a");
    let b = outer.path().join("b");
    fs::create_dir(&a).unwrap();
    cv_ok(&a, &["init"]);
    fs::write(a.join("shared.txt"), "base\n").unwrap();
    cv_ok(&a, &["save", "-m", "base"]);
    let (code, v) = cv_json(&a, &["publish"]);
    assert_eq!((code, v["command"].as_str()), (0, Some("publish")));
    assert!(v["data"]["from"].is_null() && v["data"]["published"] == true);

    let (code, v) = cv_json(
        &a,
        &["workspace", "add", b.to_str().unwrap(), "--name", "agent-b"],
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["workspace"]["name"], "agent-b");
    assert_eq!(fs::read_to_string(b.join("shared.txt")).unwrap(), "base\n");
    let (_, v) = cv_json(&b, &["workspace", "list"]);
    let list = v["data"]["workspaces"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(
        list.iter()
            .any(|w| w["name"] == "agent-b" && w["current"] == true)
    );
    assert!(
        list.iter()
            .any(|w| w["name"] == "default" && w["primary"] == true)
    );

    // A publishes; B's publish diverges (42), update merges, publish succeeds.
    fs::write(a.join("a.txt"), "a\n").unwrap();
    cv_ok(&a, &["save", "-m", "a"]);
    cv_ok(&a, &["publish"]);
    fs::write(b.join("b.txt"), "b\n").unwrap();
    cv_ok(&b, &["save", "-m", "b"]);
    let (code, v) = cv_json(&b, &["publish"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (42, Some("CONCURV_DIVERGED"))
    );
    let (code, v) = cv_json(&b, &["update"]);
    assert_eq!(
        (code, v["data"]["result"].as_str()),
        (0, Some("merged")),
        "{v}"
    );
    assert_eq!(fs::read_to_string(b.join("a.txt")).unwrap(), "a\n");
    assert_eq!(cv_json(&b, &["publish"]).0, 0);
    let (_, v) = cv_json(&a, &["update"]);
    assert_eq!(v["data"]["result"], "fast_forward");
    let (_, v) = cv_json(&a, &["update"]);
    assert_eq!(v["data"]["result"], "up_to_date");

    // Same-file conflict: 43, nothing modified.
    fs::write(a.join("shared.txt"), "from a\n").unwrap();
    cv_ok(&a, &["save", "-m", "a2"]);
    cv_ok(&a, &["publish"]);
    fs::write(b.join("shared.txt"), "from b\n").unwrap();
    cv_ok(&b, &["save", "-m", "b2"]);
    let (code, v) = cv_json(&b, &["update"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (43, Some("CONCURV_CONFLICT"))
    );
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("shared.txt")
    );
    assert_eq!(
        fs::read_to_string(b.join("shared.txt")).unwrap(),
        "from b\n"
    );
    assert_eq!(cv_json(&a, &["verify", "--full"]).0, 0);
}

/// R2 slice 2b: `undo` JSON contract and exit codes.
#[test]
fn undo_command() {
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    cv_ok(w, &["init"]);
    let (code, v) = cv_json(w, &["undo"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (50, Some("CONCURV_NOT_FOUND"))
    );
    fs::write(w.join("f"), "1\n").unwrap();
    let (_, v) = cv_json(w, &["save", "-m", "one"]);
    let s1 = v["data"]["snapshot"].as_str().unwrap().to_owned();
    fs::write(w.join("f"), "2\n").unwrap();
    cv_ok(w, &["save", "-m", "two"]);
    let (code, v) = cv_json(w, &["undo"]);
    assert_eq!((code, v["command"].as_str()), (0, Some("undo")));
    assert_eq!(v["data"]["undone_kind"], "save");
    assert_eq!(v["data"]["head"], s1.as_str());
    assert!(v["data"]["files_restored_to"].is_null());
    assert_eq!(fs::read_to_string(w.join("f")).unwrap(), "2\n");
    let (_, v) = cv_json(w, &["op", "log", "-n", "1"]);
    let op = &v["data"]["operations"][0];
    assert_eq!(op["kind"], "undo");
    assert!(op["undoes"].is_string());

    cv_ok(w, &["save", "-m", "two again"]);
    cv_ok(w, &["publish"]);
    let (code, v) = cv_json(w, &["undo"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (2, Some("CONCURV_INVALID_ARGUMENT"))
    );
    assert_eq!(cv_json(w, &["verify", "--full"]).0, 0);
}

/// R2 slice 2c: `update --allow-conflicts`, `status` merge state, resolution
/// by `save`, and `update --abort`.
#[test]
fn conflict_resolution_via_cli() {
    let outer = tempfile::tempdir().unwrap();
    let a = outer.path().join("a");
    let b = outer.path().join("b");
    fs::create_dir(&a).unwrap();
    cv_ok(&a, &["init"]);
    fs::write(a.join("f.txt"), "base\n").unwrap();
    cv_ok(&a, &["save", "-m", "base"]);
    cv_ok(&a, &["publish"]);
    cv_ok(&a, &["workspace", "add", b.to_str().unwrap()]);
    fs::write(a.join("f.txt"), "a\n").unwrap();
    cv_ok(&a, &["save", "-m", "a"]);
    cv_ok(&a, &["publish"]);
    fs::write(b.join("f.txt"), "b\n").unwrap();
    cv_ok(&b, &["save", "-m", "b"]);

    let (code, v) = cv_json(&b, &["update", "--allow-conflicts"]);
    assert_eq!(
        (code, v["data"]["result"].as_str()),
        (0, Some("conflicted")),
        "{v}"
    );
    assert_eq!(v["data"]["conflicts"][0]["path"], "f.txt");
    assert_eq!(v["data"]["conflicts"][0]["markers"], true);
    let (_, v) = cv_json(&b, &["status"]);
    assert_eq!(v["data"]["merge"]["conflicts"][0]["resolved"], false);
    let (code, v) = cv_json(&b, &["save", "-m", "early"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (43, Some("CONCURV_CONFLICT"))
    );

    // Abort, then redo the merge and resolve it.
    let (code, v) = cv_json(&b, &["update", "--abort"]);
    assert_eq!(
        (code, v["data"]["result"].as_str()),
        (0, Some("aborted")),
        "{v}"
    );
    assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "b\n");
    let (_, v) = cv_json(&b, &["status"]);
    assert!(v["data"]["merge"].is_null());
    assert_eq!(
        cv_json(&b, &["update", "--abort", "--allow-conflicts"]).0,
        2
    );

    cv_ok(&b, &["update", "--allow-conflicts"]);
    fs::write(b.join("f.txt"), "a and b\n").unwrap();
    fs::remove_file(b.join("f.txt.concurv-conflict")).unwrap();
    let human = cv_ok(&b, &["status"]);
    assert!(human.contains("All conflicts resolved"), "{human}");
    let (code, v) = cv_json(&b, &["save", "-m", "merge"]);
    assert_eq!((code, v["data"]["created"].as_bool()), (0, Some(true)));
    assert_eq!(cv_json(&b, &["publish"]).0, 0);
    cv_ok(&a, &["update"]);
    assert_eq!(fs::read_to_string(a.join("f.txt")).unwrap(), "a and b\n");
    let (code, v) = cv_json(&a, &["verify", "--full"]);
    assert_eq!(code, 0, "{v}");
}

/// R2 slice 2d: `git export` / `git import` round trip through the CLI.
#[test]
fn git_export_and_import() {
    let outer = tempfile::tempdir().unwrap();
    let a = outer.path().join("a");
    let mirror = outer.path().join("mirror.git");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&mirror).unwrap();
    let st = Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main"])
        .arg(&mirror)
        .status()
        .unwrap();
    assert!(st.success());
    cv_ok(&a, &["init"]);
    let (code, v) = cv_json(&a, &["git", "export", mirror.to_str().unwrap()]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (50, Some("CONCURV_NOT_FOUND")),
        "nothing published"
    );
    fs::write(a.join("hello.txt"), "hello\n").unwrap();
    cv_ok(&a, &["save", "-m", "hello"]);
    cv_ok(&a, &["publish"]);
    let (code, v) = cv_json(&a, &["git", "export", mirror.to_str().unwrap()]);
    assert_eq!(
        (code, v["command"].as_str()),
        (0, Some("git export")),
        "{v}"
    );
    assert_eq!(
        (
            v["data"]["exported"].as_u64(),
            v["data"]["updated"].as_bool()
        ),
        (Some(1), Some(true))
    );
    let tip = v["data"]["tip"].as_str().unwrap().to_owned();

    let b = outer.path().join("b");
    fs::create_dir(&b).unwrap();
    cv_ok(&b, &["init"]);
    let (code, v) = cv_json(&b, &["git", "import", mirror.to_str().unwrap()]);
    assert_eq!(
        (code, v["data"]["git_tip"].as_str()),
        (0, Some(tip.as_str())),
        "{v}"
    );
    assert_eq!(fs::read_to_string(b.join("hello.txt")).unwrap(), "hello\n");
    let (code, v) = cv_json(&b, &["git", "import", mirror.to_str().unwrap()]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (2, Some("CONCURV_INVALID_ARGUMENT")),
        "not fresh"
    );
    let (code, _) = cv_json(
        &b,
        &["git", "export", outer.path().join("nope").to_str().unwrap()],
    );
    assert_eq!(code, 2, "not a Git repository");
    assert_eq!(cv_json(&b, &["verify", "--full"]).0, 0);
}
