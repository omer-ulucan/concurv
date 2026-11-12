#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R5 through the CLI: `intent declare|list|release`, `overlaps`, `events`.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn run(dir: &Path, args: &[&str]) -> (i32, Value, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_concurv"))
        .args(args)
        .current_dir(dir)
        .env("CONCURV_AUTHOR", "agent-x")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    (
        o.status.code().unwrap_or(-1),
        serde_json::from_str(&out).unwrap_or(Value::Null),
        out,
    )
}

#[test]
fn intents_overlaps_and_events_through_the_cli() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a");
    fs::create_dir(&a).unwrap();
    assert_eq!(run(&a, &["init"]).0, 0);
    fs::write(a.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    assert_eq!(run(&a, &["save", "-m", "base"]).0, 0);
    assert_eq!(run(&a, &["publish"]).0, 0);
    let b = t.path().join("b");
    let (code, _, _) = run(&a, &["workspace", "add", b.to_str().unwrap()]);
    assert_eq!(code, 0);

    let (code, v, _) = run(
        &a,
        &[
            "--json",
            "intent",
            "declare",
            "f.txt",
            "--ttl",
            "45m",
            "--note",
            "editing f",
        ],
    );
    assert_eq!(code, 0, "{v}");
    let id = v["data"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        v["data"]["expires_ms"].as_i64().unwrap() - v["data"]["created_ms"].as_i64().unwrap(),
        45 * 60_000
    );
    let (_, v, _) = run(&b, &["--json", "intent", "list"]);
    assert_eq!(v["data"]["intents"][0]["id"], id.as_str());
    assert_eq!(run(&a, &["intent", "declare", "x", "--ttl", "soon"]).0, 2);

    // B edits f.txt: warned about A's intent.
    fs::write(b.join("f.txt"), "one\nTWO\nthree\n").unwrap();
    let (code, v, _) = run(&b, &["--json", "overlaps"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["overlaps"][0]["level"], "intent");
    assert_eq!(v["data"]["overlaps"][0]["path"], "f.txt");
    // Advisory: B saves anyway.
    assert_eq!(run(&b, &["save", "-m", "b"]).0, 0);
    let (_, v, _) = run(&a, &["--json", "intent", "release", &id[..8]]);
    assert_eq!(v["data"]["released"], 1);

    // Events: JSON lines in human mode, an array with --json.
    let (code, _, out) = run(&a, &["events"]);
    assert_eq!(code, 0);
    let kinds: Vec<String> = out
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        kinds,
        vec!["init", "save", "publish", "workspace-add", "update", "save"]
    );
    let (_, v, _) = run(&a, &["--json", "events", "-n", "2"]);
    assert_eq!(v["data"]["events"].as_array().unwrap().len(), 2);
}
