#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R2 gate, forced termination: real `concurv` processes in several
//! workspaces are killed at arbitrary points while saving, updating and
//! publishing. After every round the repository must verify, every save that
//! was acknowledged (exit 0 with JSON output) must still exist, and the
//! documented recovery procedure must work without touching objects by hand.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use concurv_core::{ObjectId, SnapshotId};
use concurv_workspace::graph::ancestors;
use concurv_workspace::{Repo, RepoOptions};
use serde_json::Value;

const WORKSPACES: usize = 4;
const ROUNDS: usize = 12;

fn cmd(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_concurv"));
    c.arg("--json")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("CONCURV_AUTHOR", "kill-test")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn run(dir: &Path, args: &[&str]) -> (i32, Value) {
    let o = cmd(dir, args).output().unwrap();
    let v = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
    (o.status.code().unwrap_or(-1), v)
}

/// Deterministic pseudo-random delays (xorshift).
struct Rng(u64);
impl Rng {
    fn next(&mut self, max_ms: u64) -> Duration {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        Duration::from_millis(self.0 % max_ms)
    }
}

/// Start `args` in every workspace at once, kill each after a random delay.
/// Returns the JSON documents of processes that finished successfully.
fn race(ws: &[PathBuf], args: &[&str], rng: &mut Rng, killed: &mut usize) -> Vec<(usize, Value)> {
    let children: Vec<(usize, Child)> = ws
        .iter()
        .enumerate()
        .map(|(i, d)| (i, cmd(d, args).spawn().unwrap()))
        .collect();
    let mut done = Vec::new();
    for (i, mut child) in children {
        sleep(rng.next(60));
        if child.try_wait().unwrap().is_none() {
            let _ = child.kill();
            *killed += 1;
        }
        let o = child.wait_with_output().unwrap();
        if o.status.success()
            && let Ok(v) = serde_json::from_slice::<Value>(&o.stdout)
        {
            done.push((i, v));
        }
    }
    done
}

/// The documented recovery for a workspace whose restore/checkout was killed.
fn recover(dir: &Path) {
    let (_, st) = run(dir, &["status"]);
    if let Some(target) = st["data"]["interrupted_restore"]["target"].as_str() {
        let (code, v) = run(dir, &["restore", "--snapshot", target, "--force"]);
        assert_eq!(code, 0, "recovery restore failed: {v}");
    }
}

fn snapshot(hex: &str) -> SnapshotId {
    SnapshotId::from_object_id(ObjectId::from_hex(hex).unwrap())
}

#[test]
fn killed_processes_never_lose_acknowledged_saves() {
    let outer = tempfile::tempdir().unwrap();
    let primary = outer.path().join("primary");
    fs::create_dir(&primary).unwrap();
    assert_eq!(run(&primary, &["init"]).0, 0);
    fs::write(primary.join("README"), "base").unwrap();
    assert_eq!(run(&primary, &["save", "-m", "base"]).0, 0);
    assert_eq!(run(&primary, &["publish"]).0, 0);
    let ws: Vec<PathBuf> = (0..WORKSPACES)
        .map(|i| {
            let p = outer.path().join(format!("w{i}"));
            let (code, v) = run(&primary, &["workspace", "add", p.to_str().unwrap()]);
            assert_eq!(code, 0, "{v}");
            p
        })
        .collect();

    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut acked: Vec<(usize, SnapshotId)> = Vec::new();
    let (mut killed, mut completed) = (0, 0);
    for round in 0..ROUNDS {
        for (i, d) in ws.iter().enumerate() {
            recover(d);
            fs::create_dir_all(d.join(format!("w{i}"))).unwrap();
            fs::write(d.join(format!("w{i}/r{round}.txt")), format!("{i}/{round}")).unwrap();
        }
        for (i, v) in race(
            &ws,
            &["save", "-m", &format!("round {round}")],
            &mut rng,
            &mut killed,
        ) {
            completed += 1;
            if let Some(s) = v["data"]["snapshot"].as_str() {
                acked.push((i, snapshot(s)));
            }
        }
        for d in &ws {
            recover(d);
        }
        completed += race(&ws, &["update"], &mut rng, &mut killed).len();
        for d in &ws {
            recover(d);
        }
        completed += race(&ws, &["publish"], &mut rng, &mut killed).len();

        // After every round: reopening works and nothing is corrupt.
        let (code, v) = run(&primary, &["verify", "--full"]);
        assert_eq!(code, 0, "round {round}: verify failed: {v}");
        let repo = Repo::open(&primary, RepoOptions::default()).unwrap();
        for (i, s) in &acked {
            repo.store()
                .read_snapshot(s)
                .unwrap_or_else(|e| panic!("round {round}: acked save of w{i} lost: {e}"));
        }
    }

    // Final, uninterrupted round: every workspace integrates and publishes.
    for d in &ws {
        recover(d);
        let (code, v) = run(d, &["save", "-m", "final"]);
        assert_eq!(code, 0, "{v}");
        loop {
            let (code, v) = run(d, &["update"]);
            assert_eq!(code, 0, "final update: {v}");
            match run(d, &["publish"]) {
                (0, _) => break,
                (42, _) => continue,
                (c, v) => panic!("final publish exit {c}: {v}"),
            }
        }
    }
    let repo = Repo::open(&primary, RepoOptions::default()).unwrap();
    let (_, op, _) = repo.current().unwrap();
    let main = op.view.branches["main"];
    let history = ancestors(repo.store(), main).unwrap();
    for (i, s) in &acked {
        assert!(
            history.contains_key(s),
            "acked save {s} of w{i} not in main's history"
        );
    }
    let files = repo.snapshot_files(&main).unwrap();
    for i in 0..WORKSPACES {
        for round in 0..ROUNDS {
            let p = concurv_core::RepoPath::parse(&format!("w{i}/r{round}.txt")).unwrap();
            assert!(files.contains_key(&p), "{p} missing from main");
        }
    }
    assert_eq!(run(&primary, &["verify", "--full"]).0, 0);
    eprintln!(
        "processes killed: {killed}, completed: {completed}, acknowledged saves: {}",
        acked.len()
    );
    assert!(killed > 0, "the test must actually kill processes");
}
