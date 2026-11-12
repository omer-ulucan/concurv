#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! `branch delete` and `branch rename` through the CLI (ADR 0007 §9):
//! exit codes, JSON, and workspaces following a rename.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn run(dir: &Path, args: &[&str]) -> (i32, Value, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_concurv"))
        .args(args)
        .current_dir(dir)
        .env("CONCURV_AUTHOR", "Brancher <b@example.com>")
        .env("CONCURV_CONFIG", "")
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    (
        o.status.code().unwrap_or(-1),
        serde_json::from_str(&out).unwrap_or(Value::Null),
        format!("{out}{err}"),
    )
}

#[test]
fn delete_and_rename_through_the_cli() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a");
    fs::create_dir(&a).unwrap();
    assert_eq!(run(&a, &["init"]).0, 0);
    fs::write(a.join("f.txt"), "1\n").unwrap();
    assert_eq!(run(&a, &["save", "-m", "one"]).0, 0);
    assert_eq!(run(&a, &["publish"]).0, 0);
    assert_eq!(run(&a, &["branch", "create", "feature"]).0, 0);
    let w = t.path().join("w");
    let ws = w.to_str().unwrap();
    assert_eq!(
        run(&a, &["workspace", "add", ws, "--branch", "feature"]).0,
        0
    );

    // Tracked: refused (2). Unknown: 50.
    let (code, _, out) = run(&a, &["branch", "delete", "feature"]);
    assert_eq!(code, 2, "{out}");
    assert_eq!(run(&a, &["branch", "delete", "nope"]).0, 50);

    // Rename: the workspace follows.
    let (code, v, out) = run(&a, &["--json", "branch", "rename", "feature", "feat"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(v["data"]["new_name"], "feat");
    assert_eq!(v["data"]["workspaces"][0], "w");
    let (_, v, _) = run(&w, &["--json", "status"]);
    assert_eq!(v["data"]["branch"], "feat");
    assert_eq!(run(&a, &["branch", "rename", "feat", "main"]).0, 2);

    // Unmerged work on `feat`: delete refused (42) until it is kept elsewhere.
    fs::write(w.join("g.txt"), "feature\n").unwrap();
    assert_eq!(run(&w, &["save", "-m", "feature work"]).0, 0);
    assert_eq!(run(&w, &["publish"]).0, 0);
    assert_eq!(run(&w, &["branch", "switch", "main"]).0, 0);
    let (code, _, out) = run(&a, &["branch", "delete", "feat"]);
    assert_eq!(code, 42, "{out}");
    assert_eq!(
        run(&a, &["tag", "create", "keep/feat", "--at", "feat"]).0,
        0
    );
    let (code, v, out) = run(&a, &["--json", "branch", "delete", "feat"]);
    assert_eq!(code, 0, "{out}");
    let head = v["data"]["head"].as_str().unwrap().to_owned();
    // undo refuses (2); events and the operation log record it.
    let (code, _, out) = run(&a, &["undo"]);
    assert_eq!(code, 2, "{out}");
    let (_, v, _) = run(&a, &["--json", "op", "log", "-n", "1"]);
    assert_eq!(v["data"]["operations"][0]["kind"], "branch-delete");
    assert!(
        v["data"]["operations"][0]["description"]
            .as_str()
            .unwrap()
            .contains(&head)
    );
    let (_, v, _) = run(&a, &["--json", "verify", "--full"]);
    assert_eq!(v["ok"], true);
}

/// `workspace remove` through the CLI (backlog P1-8).
#[test]
fn workspace_remove_through_the_cli() {
    let t = tempfile::tempdir().unwrap();
    let a = t.path().join("a");
    fs::create_dir(&a).unwrap();
    assert_eq!(run(&a, &["init"]).0, 0);
    fs::write(a.join("f.txt"), "1\n").unwrap();
    assert_eq!(run(&a, &["save", "-m", "one"]).0, 0);
    assert_eq!(run(&a, &["publish"]).0, 0);
    let w = t.path().join("w");
    assert_eq!(run(&a, &["workspace", "add", w.to_str().unwrap()]).0, 0);
    assert_eq!(run(&a, &["workspace", "remove", "default"]).0, 2);
    assert_eq!(run(&w, &["workspace", "remove", "w"]).0, 2);
    assert_eq!(run(&a, &["workspace", "remove", "nope"]).0, 50);
    fs::write(w.join("g.txt"), "unsaved\n").unwrap();
    assert_eq!(run(&a, &["workspace", "remove", "w"]).0, 20);
    assert_eq!(run(&w, &["save", "-m", "work"]).0, 0);
    assert_eq!(run(&a, &["workspace", "remove", "w"]).0, 42);
    assert_eq!(run(&w, &["publish"]).0, 0);
    let (code, v, out) = run(&a, &["--json", "workspace", "remove", "w"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(v["data"]["workspace"]["name"], "w");
    assert_eq!(v["data"]["directory_checked"], true);
    assert!(w.join("g.txt").is_file(), "the files stay");
    let (code, _, out) = run(&w, &["status"]);
    assert_eq!(code, 50, "{out}");
    let (_, v, _) = run(&a, &["--json", "workspace", "list"]);
    assert_eq!(v["data"]["workspaces"].as_array().unwrap().len(), 1);
}
