#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R6: `concurv config` (repository and user files), the precedence of the
//! author, the default remote, refusals, and shell completions.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// Run `concurv` with a private user configuration file and no inherited
/// author.
fn run_env(
    dir: &Path,
    user_cfg: &Path,
    env: &[(&str, &str)],
    args: &[&str],
) -> (i32, Value, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_concurv"));
    c.args(args)
        .current_dir(dir)
        .env("CONCURV_CONFIG", user_cfg)
        .env_remove("CONCURV_AUTHOR")
        .env_remove("CONCURV_REMOTE_TOKEN");
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    (
        o.status.code().unwrap_or(-1),
        serde_json::from_str(&out).unwrap_or(Value::Null),
        format!("{out}{err}"),
    )
}

fn last_author(dir: &Path, cfg: &Path) -> String {
    let (code, v, out) = run_env(dir, cfg, &[], &["--json", "log", "-n", "1"]);
    assert_eq!(code, 0, "{out}");
    v["data"]["snapshots"][0]["author"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn author_precedence_and_config_commands() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("user").join("config.json");
    let outside = t.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let run = |dir: &Path, args: &[&str]| run_env(dir, &cfg, &[], args);

    // User file, written from outside any repository.
    let (code, _, out) = run(
        &outside,
        &[
            "config",
            "set",
            "author",
            "Ada <ada@example.com>",
            "--global",
        ],
    );
    assert_eq!(code, 0, "{out}");
    let saved: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
    assert_eq!(saved["concurv_config"], 1);
    assert_eq!(saved["settings"]["author"], "Ada <ada@example.com>");
    let (code, v, _) = run(&outside, &["--json", "config", "get", "author"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["value"], "Ada <ada@example.com>");
    assert_eq!(v["data"]["source"], "user");
    // The repository file needs a repository.
    assert_eq!(run(&outside, &["config", "set", "author", "X"]).0, 10);

    let r = t.path().join("r");
    fs::create_dir(&r).unwrap();
    assert_eq!(run(&r, &["init"]).0, 0);
    fs::write(r.join("a.txt"), "1\n").unwrap();
    assert_eq!(run(&r, &["save", "-m", "one"]).0, 0);
    assert_eq!(last_author(&r, &cfg), "Ada <ada@example.com>");

    // The repository file overrides the user file, for every workspace.
    assert_eq!(
        run(
            &r,
            &["config", "set", "author", "Repo Bot <bot@example.com>"]
        )
        .0,
        0
    );
    assert!(r.join(".concurv").join("config.json").is_file());
    fs::write(r.join("a.txt"), "2\n").unwrap();
    assert_eq!(run(&r, &["save", "-m", "two"]).0, 0);
    assert_eq!(last_author(&r, &cfg), "Repo Bot <bot@example.com>");
    let w = t.path().join("w");
    assert_eq!(run(&r, &["workspace", "add", w.to_str().unwrap()]).0, 0);
    let (_, v, _) = run(&w, &["--json", "config", "get", "author"]);
    assert_eq!(v["data"]["source"], "repository");

    // The environment overrides both files.
    fs::write(r.join("a.txt"), "3\n").unwrap();
    let (code, _, _) = run_env(
        &r,
        &cfg,
        &[("CONCURV_AUTHOR", "Env <env@example.com>")],
        &["save", "-m", "three"],
    );
    assert_eq!(code, 0);
    assert_eq!(last_author(&r, &cfg), "Env <env@example.com>");

    // list shows every known key, with its source.
    let (code, v, _) = run(&r, &["--json", "config", "list"]);
    assert_eq!(code, 0);
    let keys = v["data"]["keys"].as_array().unwrap();
    let author = keys.iter().find(|k| k["key"] == "author").unwrap();
    assert_eq!(author["value"], "Repo Bot <bot@example.com>");
    assert_eq!(author["source"], "repository");
    let remote = keys.iter().find(|k| k["key"] == "remote").unwrap();
    assert!(remote["value"].is_null());

    // unset falls back to the user file; unsetting again changes nothing.
    let (code, v, _) = run(&r, &["--json", "config", "unset", "author"]);
    assert_eq!(code, 0);
    assert_eq!(v["data"]["previous"], "Repo Bot <bot@example.com>");
    let (_, v, _) = run(&r, &["--json", "config", "unset", "author"]);
    assert!(v["data"]["previous"].is_null());
    let (_, v, _) = run(&r, &["--json", "config", "get", "author"]);
    assert_eq!(v["data"]["source"], "user");

    // Refusals: unknown keys, invalid values; nothing written.
    let before = fs::read(&cfg).unwrap();
    assert_eq!(run(&r, &["config", "set", "colour", "on", "--global"]).0, 2);
    assert_eq!(
        run(&r, &["config", "set", "author", "a\nb", "--global"]).0,
        2
    );
    assert_eq!(run(&r, &["config", "set", "remote", "-x", "--global"]).0, 2);
    assert_eq!(run(&r, &["config", "get", "nope"]).0, 2);
    assert_eq!(fs::read(&cfg).unwrap(), before);
    // get of an unset key: not found.
    assert_eq!(run(&r, &["config", "get", "remote"]).0, 50);

    // A malformed file stops every command with its path, changing nothing.
    fs::write(r.join(".concurv").join("config.json"), "{not json").unwrap();
    let (code, _, out) = run(&r, &["status"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("config.json"), "{out}");
    fs::write(
        r.join(".concurv").join("config.json"),
        r#"{"concurv_config": 1, "settings": {"editor": "vi"}}"#,
    )
    .unwrap();
    let (code, _, out) = run(&r, &["save", "-m", "x"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("editor"), "{out}");
    fs::remove_file(r.join(".concurv").join("config.json")).unwrap();
    assert_eq!(run(&r, &["status"]).0, 0);
}

#[test]
fn configured_default_remote() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("config.json");
    let r = t.path().join("r");
    fs::create_dir(&r).unwrap();
    let token = [("CONCURV_REMOTE_TOKEN", "tok")];
    let run = |args: &[&str]| run_env(&r, &cfg, &token, args);
    assert_eq!(run(&["init"]).0, 0);
    let fp = format!("sha256:{}", "ab".repeat(32));
    // Two remotes, neither named origin; port 1 refuses connections.
    for name in ["a", "b"] {
        let (code, _, out) = run(&[
            "remote",
            "add",
            name,
            "concurv://127.0.0.1:1",
            "--fingerprint",
            &fp,
        ]);
        assert_eq!(code, 0, "{out}");
    }
    let (code, _, out) = run(&["fetch"]);
    assert_eq!(code, 2, "ambiguous without configuration: {out}");
    assert!(out.contains("several remotes"), "{out}");
    assert_eq!(run(&["config", "set", "remote", "b"]).0, 0);
    let (code, v, _) = run(&["--json", "fetch"]);
    assert_eq!(code, 70, "the configured remote is contacted: {v}");
    // A configured remote that does not exist is reported as such.
    assert_eq!(run(&["config", "set", "remote", "gone"]).0, 0);
    assert_eq!(run(&["fetch"]).0, 50);
    // The command line still wins.
    assert_eq!(run(&["fetch", "a"]).0, 70);
}

#[test]
fn completions_for_every_supported_shell() {
    let t = tempfile::tempdir().unwrap();
    let cfg = t.path().join("config.json");
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let (code, _, out) = run_env(t.path(), &cfg, &[], &["completions", shell]);
        assert_eq!(code, 0, "{shell}: {out}");
        for word in ["concurv", "publish", "intent", "config"] {
            assert!(out.contains(word), "{shell} completion lacks {word}");
        }
    }
    assert_eq!(run_env(t.path(), &cfg, &[], &["completions", "tcsh"]).0, 2);
}
