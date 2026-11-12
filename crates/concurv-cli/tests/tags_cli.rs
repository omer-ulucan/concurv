#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R6 through the CLI: `tag create|list|delete` (ADR 0008), `branch create
//! --at <tag>`, and `git export --tags` / `git import --into --tags`
//! (needs `git` on PATH).

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn run(dir: &Path, args: &[&str]) -> (i32, Value, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_concurv"))
        .args(args)
        .current_dir(dir)
        .env("CONCURV_AUTHOR", "Tagger <t@example.com>")
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

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.autocrlf=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_AUTHOR_NAME", "Grace")
        .env("GIT_AUTHOR_EMAIL", "grace@example.com")
        .env("GIT_COMMITTER_NAME", "Grace")
        .env("GIT_COMMITTER_EMAIL", "grace@example.com")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

#[test]
fn tag_commands_and_exit_codes() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("r");
    fs::create_dir(&r).unwrap();
    assert_eq!(run(&r, &["init"]).0, 0);
    assert_eq!(run(&r, &["tag", "create", "v0"]).0, 2, "nothing to tag");
    fs::write(r.join("f.txt"), "1\n").unwrap();
    assert_eq!(run(&r, &["save", "-m", "one"]).0, 0);
    assert_eq!(run(&r, &["publish"]).0, 0);

    let (code, v, out) = run(&r, &["--json", "tag", "create", "v1.0"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(v["data"]["format_upgraded"], true);
    let target = v["data"]["target"].as_str().unwrap().to_owned();
    assert_eq!(
        fs::read_to_string(r.join(".concurv").join("format")).unwrap(),
        "concurv-repo-format 4\n"
    );
    // Tags never move; collisions and bad names are refused.
    assert_eq!(run(&r, &["tag", "create", "v1.0"]).0, 2);
    assert_eq!(run(&r, &["tag", "create", "V1.0"]).0, 2);
    assert_eq!(run(&r, &["tag", "create", "bad name"]).0, 2);
    // --at a tag, from tag and branch commands.
    let (code, v, _) = run(
        &r,
        &["--json", "branch", "create", "hotfix", "--at", "v1.0"],
    );
    assert_eq!(code, 0);
    assert_eq!(v["data"]["head"], target.as_str());
    fs::write(r.join("f.txt"), "2\n").unwrap();
    assert_eq!(run(&r, &["save", "-m", "two"]).0, 0);
    assert_eq!(run(&r, &["tag", "create", "hotfix"]).0, 0);
    let (code, _, out) = run(&r, &["tag", "create", "x", "--at", "hotfix"]);
    assert_eq!(code, 51, "a branch and a tag of that name differ: {out}");

    let (_, v, _) = run(&r, &["--json", "tag", "list"]);
    let names: Vec<&str> = v["data"]["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["hotfix", "v1.0"]);
    // undo refuses tag operations (exit 2), nothing changes.
    let (code, _, out) = run(&r, &["undo"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("tag-create"), "{out}");
    let (code, v, _) = run(&r, &["--json", "tag", "delete", "v1.0"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["target"], target.as_str());
    assert_eq!(run(&r, &["tag", "delete", "v1.0"]).0, 50);
    assert_eq!(run(&r, &["tag", "create", "v1.0", "--at", &target]).0, 0);
    let (_, v, _) = run(&r, &["--json", "verify", "--full"]);
    assert_eq!(v["ok"], true);
    // Events name the tag operations.
    let (_, _, out) = run(&r, &["events"]);
    assert!(out.contains("\"tag-create\"") && out.contains("\"tag-delete\""));
}

#[test]
fn git_export_tags_and_import_into_existing_history() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path().join("r");
    fs::create_dir(&r).unwrap();
    assert_eq!(run(&r, &["init"]).0, 0);
    fs::write(r.join("f.txt"), "1\n").unwrap();
    assert_eq!(run(&r, &["save", "-m", "one"]).0, 0);
    assert_eq!(run(&r, &["publish"]).0, 0);
    assert_eq!(run(&r, &["tag", "create", "v1"]).0, 0);

    let g = t.path().join("mirror.git");
    fs::create_dir(&g).unwrap();
    git(&g, &["init", "-q", "--bare", "-b", "main"]);
    let gs = g.to_str().unwrap();
    let (code, v, out) = run(&r, &["--json", "git", "export", gs, "--tags"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(v["data"]["tags"][0]["name"], "v1");
    assert_eq!(v["data"]["tags"][0]["created"], true);
    assert_eq!(git(&g, &["tag", "--list"]), "v1");

    // Someone continues in Git.
    let work = t.path().join("work");
    git(t.path(), &["clone", "-q", gs, work.to_str().unwrap()]);
    fs::write(work.join("g.txt"), "from git\n").unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "git work"]);
    git(&work, &["tag", "v2"]);
    git(&work, &["push", "-q", "--tags", "origin", "main"]);

    // concurv picks the new commit and tag up on top of its own history.
    let (code, v, out) = run(&r, &["--json", "git", "import", gs, "--tags"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(v["data"]["commits"], 1);
    assert_eq!(v["data"]["published"], true);
    assert_eq!(v["data"]["checked_out"], true);
    assert_eq!(v["data"]["tags_created"][0]["name"], "v2");
    assert_eq!(v["data"]["tags_unchanged"], 1);
    assert_eq!(fs::read_to_string(r.join("g.txt")).unwrap(), "from git\n");
    // Into another branch, by name.
    let (code, v, _) = run(&r, &["--json", "git", "import", gs, "--into", "mirror"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["into"], "mirror");
    assert_eq!(v["data"]["checked_out"], false);
    let (_, v, _) = run(&r, &["--json", "branch", "list"]);
    assert!(
        v["data"]["branches"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["name"] == "mirror")
    );
}
