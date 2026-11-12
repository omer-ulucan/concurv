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
use std::time::{Duration, Instant};

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

/// Deterministic pseudo-random delays (xorshift), microsecond resolution.
struct Rng(u64);
impl Rng {
    fn next(&mut self, max: Duration) -> Duration {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let max_us = u64::try_from(max.as_micros()).unwrap_or(u64::MAX).max(1);
        Duration::from_micros(self.0 % max_us)
    }
}

/// Start `args` in every workspace at once and kill each at a random moment
/// within `span` of the start. `span` is calibrated to how long the commands
/// take on this machine, so kills land mid-command on fast and slow machines
/// alike. Returns the JSON documents of processes that finished successfully.
fn race(
    ws: &[PathBuf],
    args: &[&str],
    rng: &mut Rng,
    span: Duration,
    killed: &mut usize,
) -> Vec<(usize, Value)> {
    let start = Instant::now();
    let children: Vec<(usize, Child, Duration)> = ws
        .iter()
        .enumerate()
        .map(|(i, d)| (i, cmd(d, args).spawn().unwrap(), rng.next(span)))
        .collect();
    let mut done = Vec::new();
    for (i, mut child, deadline) in children {
        if let Some(rest) = deadline.checked_sub(start.elapsed()) {
            sleep(rest);
        }
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

    // Calibrate: how long all workspaces take to save, update and publish at
    // once, without kills. Kill moments are drawn from that span.
    let mut acked: Vec<(usize, SnapshotId)> = Vec::new();
    for (i, d) in ws.iter().enumerate() {
        fs::write(d.join(format!("calibration{i}.txt")), "c").unwrap();
    }
    let mut span = Duration::ZERO;
    for args in [
        &["save", "-m", "calibration"][..],
        &["update"],
        &["publish"],
    ] {
        let t = Instant::now();
        let children: Vec<Child> = ws.iter().map(|d| cmd(d, args).spawn().unwrap()).collect();
        for (i, child) in children.into_iter().enumerate() {
            let o = child.wait_with_output().unwrap();
            if let Ok(v) = serde_json::from_slice::<Value>(&o.stdout)
                && let Some(s) = v["data"]["snapshot"].as_str()
            {
                acked.push((i, snapshot(s)));
            }
        }
        span = span.max(t.elapsed());
    }
    let span = (span * 5 / 4).max(Duration::from_millis(5));
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
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
            span,
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
        completed += race(&ws, &["update"], &mut rng, span, &mut killed).len();
        for d in &ws {
            recover(d);
        }
        completed += race(&ws, &["publish"], &mut rng, span, &mut killed).len();

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
        "kill span {span:?}; processes killed: {killed}, completed: {completed}, acknowledged saves: {}",
        acked.len()
    );
    assert!(killed > 0, "the test must actually kill processes");
}

/// ADR 0007 §3: `branch switch` processes killed at random points. After
/// each kill, the documented recovery (`status`, then the same `branch
/// switch` again) must give a consistent workspace: tracking one branch,
/// files equal to that branch's head, the untracked file intact, and the
/// repository verifying.
#[test]
fn killed_branch_switches_are_finished_by_running_them_again() {
    const FILES: usize = 300;
    let outer = tempfile::tempdir().unwrap();
    let a = outer.path().join("a");
    fs::create_dir(&a).unwrap();
    assert_eq!(run(&a, &["init"]).0, 0);
    for i in 0..FILES {
        fs::create_dir_all(a.join(format!("d{}", i % 10))).unwrap();
        fs::write(
            a.join(format!("d{}/f{i}.txt", i % 10)),
            format!("main {i}\n"),
        )
        .unwrap();
    }
    assert_eq!(run(&a, &["save", "-m", "main"]).0, 0);
    assert_eq!(run(&a, &["publish"]).0, 0);
    assert_eq!(run(&a, &["branch", "create", "feature", "--switch"]).0, 0);
    for i in 0..FILES {
        fs::write(
            a.join(format!("d{}/f{i}.txt", i % 10)),
            format!("feature {i}\n"),
        )
        .unwrap();
        if i % 7 == 0 {
            fs::write(a.join(format!("d{}/extra{i}.txt", i % 10)), "extra\n").unwrap();
        }
    }
    assert_eq!(run(&a, &["save", "-m", "feature"]).0, 0);
    assert_eq!(run(&a, &["publish"]).0, 0);
    fs::write(a.join("notes.txt"), "untracked notes").unwrap();
    // Calibrate: kill anywhere within the duration of one full switch.
    let t = std::time::Instant::now();
    assert_eq!(run(&a, &["branch", "switch", "main"]).0, 0);
    let span = (t.elapsed() * 5 / 4).max(Duration::from_millis(20));

    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut killed, mut resumed) = (0, 0);
    for round in 0..16 {
        let (_, st) = run(&a, &["status"]);
        let on = st["data"]["branch"].as_str().unwrap().to_owned();
        let to = if on == "main" { "feature" } else { "main" };
        let mut child = cmd(&a, &["branch", "switch", to]).spawn().unwrap();
        sleep(rng.next(span));
        if child.try_wait().unwrap().is_none() {
            let _ = child.kill();
            killed += 1;
        }
        let _ = child.wait_with_output().unwrap();

        // Documented recovery.
        let (_, st) = run(&a, &["status"]);
        if let Some(b) = st["data"]["interrupted_restore"]["branch"].as_str() {
            let (code, v) = run(&a, &["branch", "switch", b]);
            assert_eq!(code, 0, "round {round}: resuming failed: {v}");
            assert_eq!(v["data"]["resumed"], true, "round {round}: {v}");
            resumed += 1;
        }
        let (code, st) = run(&a, &["status"]);
        assert_eq!(code, 0);
        assert!(
            st["data"]["interrupted_restore"].is_null(),
            "round {round}: {st}"
        );
        let changes = st["data"]["changes"].as_array().unwrap();
        assert_eq!(changes.len(), 1, "round {round}: only notes.txt: {st}");
        assert_eq!(changes[0]["path"], "notes.txt");
        assert_eq!(
            fs::read_to_string(a.join("notes.txt")).unwrap(),
            "untracked notes"
        );
        let on = st["data"]["branch"].as_str().unwrap();
        let sample = fs::read_to_string(a.join("d3/f123.txt")).unwrap();
        assert_eq!(sample, format!("{on} 123\n"), "round {round}");
        assert_eq!(
            a.join("d0/extra0.txt").exists(),
            on == "feature",
            "round {round}"
        );
        let (code, v) = run(&a, &["verify", "--full"]);
        assert_eq!(code, 0, "round {round}: {v}");
    }
    eprintln!(
        "switch span {span:?}; processes killed: {killed}, interrupted switches resumed: {resumed}"
    );
    assert!(killed > 0, "the test must actually kill processes");
    assert!(resumed > 0, "some kill must land inside a switch");
}
