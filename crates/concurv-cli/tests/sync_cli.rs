#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]
//! R3 gate, local two-process harness: a real `concurvd` process and real
//! `concurv` processes in independent repositories, talking TLS over
//! loopback. Covers the documented workflow and its exit codes, and kills
//! of the relay and of clients in the middle of pushes.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use concurv_core::{ObjectId, SnapshotId};
use concurv_sync::{Relay, RelayOptions};
use serde_json::Value;

const CONCURV: &str = env!("CARGO_BIN_EXE_concurv");
const CONCURVD: &str = env!("CARGO_BIN_EXE_concurvd");

fn cmd(bin: &str, dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(bin);
    c.arg("--json")
        .args(args)
        .current_dir(dir)
        .env("CONCURV_AUTHOR", "sync-cli-test")
        .env_remove("CONCURV_REMOTE_TOKEN")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn run(bin: &str, dir: &Path, args: &[&str]) -> (i32, Value) {
    let o = cmd(bin, dir, args).output().unwrap();
    let v = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
    (o.status.code().unwrap_or(-1), v)
}

fn ok(bin: &str, dir: &Path, args: &[&str]) -> Value {
    let (code, v) = run(bin, dir, args);
    assert_eq!(code, 0, "{args:?}: {v}");
    v["data"].clone()
}

fn snapshot(hex: &str) -> SnapshotId {
    SnapshotId::from_object_id(ObjectId::from_hex(hex).unwrap())
}

/// A `concurvd serve` process.
struct RelayProc {
    dir: PathBuf,
    port: u16,
    child: Option<Child>,
}

impl RelayProc {
    fn start(dir: &Path, port: u16) -> Self {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let mut child = cmd(
                CONCURVD,
                dir.parent().unwrap(),
                &[
                    "serve",
                    dir.to_str().unwrap(),
                    "--listen",
                    &format!("127.0.0.1:{port}"),
                ],
            )
            .spawn()
            .unwrap();
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && let Some(addr) = v["data"]["listening"].as_str()
            {
                let port = addr.rsplit(':').next().unwrap().parse().unwrap();
                return Self {
                    dir: dir.to_path_buf(),
                    port,
                    child: Some(child),
                };
            }
            // Port still busy after a kill: try again.
            let _ = child.wait();
            assert!(Instant::now() < deadline, "relay did not start: {line}");
            sleep(Duration::from_millis(100));
        }
    }

    fn kill(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn restart(&mut self) {
        self.kill();
        *self = Self::start(&self.dir.clone(), self.port);
    }
}

impl Drop for RelayProc {
    fn drop(&mut self) {
        self.kill();
    }
}

struct Setup {
    t: tempfile::TempDir,
    relay_dir: PathBuf,
    fingerprint: String,
    relay: RelayProc,
}

impl Setup {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let relay_dir = t.path().join("relay");
        let fingerprint =
            ok(CONCURVD, t.path(), &["init", relay_dir.to_str().unwrap()])["fingerprint"]
                .as_str()
                .unwrap()
                .to_owned();
        for (name, scopes) in [
            ("alice", &[][..]),
            ("bob", &[][..]),
            ("reader", &["--scope", "read"][..]),
        ] {
            let mut args = vec!["device", "add", relay_dir.to_str().unwrap(), name];
            args.extend_from_slice(scopes);
            let tok = ok(CONCURVD, t.path(), &args)["token"]
                .as_str()
                .unwrap()
                .to_owned();
            fs::write(t.path().join(format!("{name}.tok")), tok).unwrap();
        }
        let relay = RelayProc::start(&relay_dir, 0);
        Self {
            t,
            relay_dir,
            fingerprint,
            relay,
        }
    }

    fn url(&self) -> String {
        format!("concurv://127.0.0.1:{}", self.relay.port)
    }

    fn token(&self, who: &str) -> String {
        self.t
            .path()
            .join(format!("{who}.tok"))
            .to_str()
            .unwrap()
            .to_owned()
    }

    fn clone(&self, name: &str, who: &str) -> PathBuf {
        let url = self.url();
        let tok = self.token(who);
        ok(
            CONCURV,
            self.t.path(),
            &[
                "clone",
                &url,
                name,
                "--fingerprint",
                &self.fingerprint,
                "--token-file",
                &tok,
            ],
        );
        self.t.path().join(name)
    }

    fn relay_head(&self) -> Option<SnapshotId> {
        Relay::open(&self.relay_dir, RelayOptions::default())
            .unwrap()
            .heads()
            .unwrap()
            .get("main")
            .map(|h| h.head)
    }

    /// The relay's history of main contains `snaps`, and every head's graph
    /// is complete and valid.
    fn relay_has(&self, snaps: &[SnapshotId]) {
        let relay = Relay::open(&self.relay_dir, RelayOptions::default()).unwrap();
        let Some(h) = relay.heads().unwrap().get("main").map(|h| h.head) else {
            assert!(snaps.is_empty());
            return;
        };
        for id in concurv_sync::graph::objects_to_send(relay.store(), &[h], &[]).unwrap() {
            relay.store().verify(&id).unwrap();
        }
        let hist = concurv_workspace::graph::ancestors(relay.store(), h).unwrap();
        for s in snaps {
            assert!(hist.contains_key(s), "{s} missing from the relay's main");
        }
    }
}

fn save_publish(dir: &Path, file: &str, content: &str, msg: &str) -> SnapshotId {
    let p = dir.join(file);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
    let s = ok(CONCURV, dir, &["save", "-m", msg])["snapshot"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(CONCURV, dir, &["publish"]);
    snapshot(&s)
}

#[test]
fn two_repositories_sync_through_a_relay_process() {
    let mut s = Setup::new();
    let a = s.clone("a", "alice");
    save_publish(&a, "README", "base\n", "base");
    let p = ok(CONCURV, &a, &["push"]);
    assert_eq!(p["result"], "pushed");
    assert_eq!(p["seq"], 1);
    let b = s.clone("b", "bob");
    assert_eq!(fs::read_to_string(b.join("README")).unwrap(), "base\n");

    let a1 = save_publish(&a, "a.txt", "a\n", "a1");
    let b1 = save_publish(&b, "b.txt", "b\n", "b1");
    assert_eq!(ok(CONCURV, &a, &["push", "origin"])["result"], "pushed");
    let (code, v) = run(CONCURV, &b, &["push"]);
    assert_eq!(code, 42, "{v}");
    assert_eq!(v["error"]["code"], "CONCURV_DIVERGED");
    let f = ok(CONCURV, &b, &["fetch"]);
    assert_eq!(f["branches"][0]["branch"], "main");
    assert_eq!(f["branches"][0]["action"], "diverged");
    assert_eq!(f["branches"][0]["remote_head"], a1.to_hex());
    let m = ok(CONCURV, &b, &["merge", "remote/origin/main"]);
    assert_eq!(m["result"], "merged");
    ok(CONCURV, &b, &["publish"]);
    let p = ok(CONCURV, &b, &["push"]);
    assert_eq!(
        (p["result"].as_str(), p["seq"].as_u64()),
        (Some("pushed"), Some(3))
    );
    s.relay_has(&[a1, b1]);

    let f = ok(CONCURV, &a, &["fetch"]);
    assert_eq!(f["branches"][0]["action"], "fast_forwarded");
    assert_eq!(ok(CONCURV, &a, &["update"])["result"], "fast_forward");
    for d in [&a, &b] {
        assert_eq!(fs::read_to_string(d.join("a.txt")).unwrap(), "a\n");
        assert_eq!(fs::read_to_string(d.join("b.txt")).unwrap(), "b\n");
        assert_eq!(run(CONCURV, d, &["verify", "--full"]).0, 0);
    }
    // Remote-tracking references in `remote list` and `op log`.
    let r = ok(CONCURV, &a, &["remote", "list"]);
    assert_eq!(r["remotes"][0]["name"], "origin");
    assert_eq!(
        r["remotes"][0]["branches"]["main"]["head"].as_str(),
        s.relay_head().map(|h| h.to_hex()).as_deref()
    );
    let log = ok(CONCURV, &b, &["op", "log", "-n", "20"]);
    let kinds: Vec<&str> = log["operations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"fetch") && kinds.contains(&"push"),
        "{kinds:?}"
    );
    assert!(log["operations"][0]["remotes"]["origin"]["main"]["seq"].is_u64());

    // Refusals and their exit codes.
    let c = s.t.path().join("c");
    fs::create_dir(&c).unwrap();
    ok(CONCURV, &c, &["init"]);
    let wrong_fp = format!("sha256:{}", "ab".repeat(32));
    let url = s.url();
    let alice = s.token("alice");
    ok(
        CONCURV,
        &c,
        &[
            "remote",
            "add",
            "evil",
            &url,
            "--fingerprint",
            &wrong_fp,
            "--token-file",
            &alice,
        ],
    );
    let (code, v) = run(CONCURV, &c, &["fetch", "evil"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (75, Some("CONCURV_RELAY_IDENTITY"))
    );
    let bad_tok = s.t.path().join("bad.tok");
    fs::write(&bad_tok, format!("cvt1_{}", "0".repeat(64))).unwrap();
    ok(
        CONCURV,
        &c,
        &[
            "remote",
            "add",
            "origin",
            &url,
            "--fingerprint",
            &s.fingerprint,
            "--token-file",
            bad_tok.to_str().unwrap(),
        ],
    );
    let (code, v) = run(CONCURV, &c, &["fetch", "origin"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (73, Some("CONCURV_AUTH_FAILED"))
    );
    let (code, _) = run(CONCURV, &c, &["fetch"]);
    assert_eq!(code, 73, "default remote is origin when several exist");
    let r = s.clone("r", "reader");
    save_publish(&r, "r.txt", "r\n", "r1");
    let (code, v) = run(CONCURV, &r, &["push"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (74, Some("CONCURV_PERMISSION_DENIED"))
    );
    let (code, _) = run(CONCURV, &r, &["fetch", "nope"]);
    assert_eq!(code, 50);
    s.relay.kill();
    let (code, v) = run(CONCURV, &a, &["fetch"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (70, Some("CONCURV_NETWORK"))
    );
    s.relay.restart();
    assert_eq!(run(CONCURV, &a, &["fetch"]).0, 0);
    // Removing a remote drops its references; local branches stay.
    let rm = ok(CONCURV, &a, &["remote", "remove", "origin"]);
    assert!(rm["operation"].is_string());
    assert!(
        ok(CONCURV, &a, &["remote", "list"])["remotes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(run(CONCURV, &a, &["verify", "--full"]).0, 0);
}

/// Deterministic pseudo-random delays (xorshift).
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

/// Clients push at the same time while the relay process (even rounds) or
/// client processes (odd rounds) are killed at random moments. Every push
/// that reported success is on the relay, the relay's graph stays complete
/// and valid across restarts, every client repository verifies, and in the
/// end every acknowledged save reaches the relay.
#[test]
fn killed_relay_and_killed_clients_never_lose_acknowledged_work() {
    const CLIENTS: usize = 3;
    const ROUNDS: usize = 8;
    let mut s = Setup::new();
    let first = s.clone("c0", "alice");
    save_publish(&first, "README", "base\n", "base");
    ok(CONCURV, &first, &["push"]);
    let mut clients = vec![first];
    for i in 1..CLIENTS {
        clients.push(s.clone(&format!("c{i}"), if i % 2 == 0 { "alice" } else { "bob" }));
    }
    let mut saves = Vec::new();
    let mut pushed = Vec::new();

    // Calibrate: one round of concurrent pushes without kills.
    for (i, c) in clients.iter().enumerate() {
        saves.push(save_publish(
            c,
            &format!("w{i}/calibration.txt"),
            "c\n",
            "calibration",
        ));
    }
    let t = Instant::now();
    let children: Vec<Child> = clients
        .iter()
        .map(|c| cmd(CONCURV, c, &["push"]).spawn().unwrap())
        .collect();
    for ch in children {
        let _ = ch.wait_with_output();
    }
    let span = (t.elapsed() * 5 / 4).max(Duration::from_millis(20));
    let converge = |c: &Path| {
        for _ in 0..20 {
            let (code, v) = run(CONCURV, c, &["push"]);
            match code {
                0 => return v["data"].clone(),
                41 | 42 | 70 => {
                    if run(CONCURV, c, &["fetch"]).0 != 0 {
                        sleep(Duration::from_millis(100));
                        continue;
                    }
                    let (mc, mv) = run(CONCURV, c, &["merge", "remote/origin/main"]);
                    assert!(mc == 0 || mc == 2, "{mv}");
                    if mc != 0 {
                        // Already tracking the fetched head: plain update.
                        assert_eq!(run(CONCURV, c, &["update"]).0, 0);
                    }
                    let (pc, pv) = run(CONCURV, c, &["publish"]);
                    assert!(pc == 0, "{pv}");
                }
                _ => panic!("push exit {code}: {v}"),
            }
        }
        panic!("did not converge");
    };
    for c in &clients {
        converge(c);
    }

    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    let (mut relay_kills, mut client_kills) = (0, 0);
    for round in 0..ROUNDS {
        for (i, c) in clients.iter().enumerate() {
            saves.push(save_publish(
                c,
                &format!("w{i}/r{round}.txt"),
                &format!("{i}/{round}\n"),
                "work",
            ));
        }
        let start = Instant::now();
        let mut children: Vec<Child> = clients
            .iter()
            .map(|c| cmd(CONCURV, c, &["push"]).spawn().unwrap())
            .collect();
        // Clients are killed early in their push (all that still run), the
        // relay anywhere in the round.
        let kill_at = rng.next(if round % 2 == 0 { span } else { span / 2 });
        sleep(kill_at.saturating_sub(start.elapsed()));
        if round % 2 == 0 {
            s.relay.kill();
            relay_kills += 1;
        } else {
            for ch in children.iter_mut() {
                if ch.try_wait().unwrap().is_none() {
                    let _ = ch.kill();
                    client_kills += 1;
                }
            }
        }
        for ch in children {
            let o = ch.wait_with_output().unwrap();
            if o.status.success()
                && let Ok(v) = serde_json::from_slice::<Value>(&o.stdout)
                && v["data"]["result"] == "pushed"
            {
                pushed.push(snapshot(v["data"]["head"].as_str().unwrap()));
            }
        }
        if round % 2 == 0 {
            s.relay.restart();
        }
        // After every round: the relay has every acknowledged push and a
        // complete graph; every client verifies.
        s.relay_has(&pushed);
        for c in &clients {
            let (code, v) = run(CONCURV, c, &["verify", "--full"]);
            assert_eq!(code, 0, "round {round}: {v}");
        }
        // Integrate what the others pushed.
        for c in &clients {
            let d = converge(c);
            if d["result"] == "pushed" {
                pushed.push(snapshot(d["head"].as_str().unwrap()));
            }
        }
    }
    for c in &clients {
        converge(c);
    }
    s.relay_has(&saves);
    s.relay_has(&pushed);
    eprintln!(
        "kill span {span:?}; relay kills {relay_kills}, client kills {client_kills}, acknowledged pushes {}",
        pushed.len()
    );
    assert!(
        relay_kills > 0 && client_kills > 0,
        "the test must kill processes"
    );
}
