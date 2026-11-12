#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]
//! Shared helpers for sync tests: a relay served over TLS on loopback, and
//! client repositories that know it as `origin`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use concurv_core::SnapshotId;
use concurv_objects::Faults;
use concurv_sync::relay::Head;
use concurv_sync::{
    ConnectOptions, Relay, RelayOptions, Scope, Server, ServerHandle, ServerOptions, SyncOptions,
};
use concurv_workspace::{Repo, RepoOptions};

pub struct Env {
    pub dir: tempfile::TempDir,
    pub relay_dir: PathBuf,
    pub server: Option<ServerHandle>,
    pub fingerprint: String,
    pub url: String,
    /// read, write, publish:*
    pub alice: String,
    /// read, write, publish:*
    pub bob: String,
    /// read only
    pub reader: String,
    /// read, write, publish:main
    pub main_only: String,
}

pub fn server_opts() -> ServerOptions {
    ServerOptions {
        max_connections: 64,
        idle_timeout: Duration::from_secs(30),
        auth_failure_delay: Duration::ZERO,
    }
}

impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let relay_dir = dir.path().join("relay");
        let init = Relay::init(&relay_dir).unwrap();
        let relay = Relay::open(&relay_dir, RelayOptions::default()).unwrap();
        let all = [Scope::Read, Scope::Write, Scope::Publish(None)];
        let alice = relay.add_device("alice", &all).unwrap();
        let bob = relay.add_device("bob", &all).unwrap();
        let reader = relay.add_device("reader", &[Scope::Read]).unwrap();
        let main_only = relay
            .add_device(
                "main-only",
                &[
                    Scope::Read,
                    Scope::Write,
                    Scope::Publish(Some("main".into())),
                ],
            )
            .unwrap();
        let server = Server::bind(relay, "127.0.0.1:0", server_opts())
            .unwrap()
            .spawn()
            .unwrap();
        let url = format!("concurv://127.0.0.1:{}", server.addr().port());
        Self {
            dir,
            relay_dir,
            server: Some(server),
            fingerprint: init.fingerprint,
            url,
            alice,
            bob,
            reader,
            main_only,
        }
    }

    /// A second handle on the relay directory (inspection, direct calls).
    pub fn relay(&self) -> Relay {
        Relay::open(&self.relay_dir, RelayOptions::default()).unwrap()
    }

    pub fn heads(&self) -> BTreeMap<String, Head> {
        self.relay().heads().unwrap()
    }

    pub fn head(&self, branch: &str) -> Option<SnapshotId> {
        self.heads().get(branch).map(|h| h.head)
    }

    /// A new, empty client repository with `origin` = this relay.
    pub fn client(&self, name: &str, token: &str) -> Repo {
        self.client_with(name, token, Faults::none())
    }

    pub fn client_with(&self, name: &str, token: &str, faults: Faults) -> Repo {
        let root = self.dir.path().join(name);
        fs::create_dir_all(&root).unwrap();
        let repo = Repo::init(&root, opts(faults)).unwrap();
        concurv_sync::remote::add(&repo, "origin", &self.url, &self.fingerprint, token).unwrap();
        repo
    }

    /// Clone the relay into a new directory.
    pub fn clone(&self, name: &str, token: &str) -> Repo {
        let root = self.dir.path().join(name);
        concurv_sync::clone_repo(
            &root,
            &self.url,
            &self.fingerprint,
            token,
            "origin",
            opts(Faults::none()),
            &sync_opts(),
        )
        .unwrap();
        Repo::open(&root, opts(Faults::none())).unwrap()
    }
}

pub fn opts(faults: Faults) -> RepoOptions {
    RepoOptions {
        faults,
        now_ms: Some(1_793_491_200_000),
        author: Some("Sync Test <sync@example.com>".into()),
        lock_timeout: None,
        stat_cache_window: None,
    }
}

pub fn sync_opts() -> SyncOptions {
    SyncOptions {
        connect: ConnectOptions {
            timeout: Duration::from_secs(20),
            ..ConnectOptions::default()
        },
        retries: 2,
    }
}

pub fn no_retry() -> SyncOptions {
    SyncOptions {
        retries: 0,
        ..sync_opts()
    }
}

pub fn write(repo: &Repo, path: &str, content: &str) {
    let p = repo.root().join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

pub fn read(repo: &Repo, path: &str) -> String {
    fs::read_to_string(repo.root().join(path)).unwrap()
}

/// Save and publish to the local branch; returns the snapshot.
pub fn save_publish(repo: &Repo, msg: &str) -> SnapshotId {
    let s = concurv_workspace::save(repo, msg).unwrap();
    concurv_workspace::publish(repo).unwrap();
    s.snapshot.expect("something to save")
}

pub fn local_branch(repo: &Repo, b: &str) -> Option<SnapshotId> {
    repo.oplog().head().unwrap().1.view.branches.get(b).copied()
}

pub fn tracking(repo: &Repo, b: &str) -> Option<SnapshotId> {
    repo.oplog()
        .head()
        .unwrap()
        .1
        .view
        .remotes
        .get("origin")
        .and_then(|m| m.get(b))
        .map(|r| r.head)
}

/// Every snapshot in `snaps` is in the relay's history of `branch`.
pub fn relay_history_contains(env: &Env, branch: &str, snaps: &[SnapshotId]) {
    let relay = env.relay();
    let head = relay.heads().unwrap()[branch].head;
    let hist = concurv_workspace::graph::ancestors(relay.store(), head).unwrap();
    for s in snaps {
        assert!(hist.contains_key(s), "{s} is not in the relay's {branch}");
    }
}

/// The relay's store holds the complete, valid graph of every head.
pub fn relay_is_consistent(relay_dir: &Path) {
    let relay = Relay::open(relay_dir, RelayOptions::default()).unwrap();
    for (b, h) in relay.heads().unwrap() {
        let missing = concurv_sync::graph::staged_closure(
            &concurv_objects::ObjectStore::new(&relay_dir.join("nowhere"), Faults::none()),
            relay.store(),
            &[h.head],
        )
        .unwrap();
        assert!(missing.missing.is_empty(), "{b}: head not stored");
        let all = concurv_sync::graph::objects_to_send(relay.store(), &[h.head], &[]).unwrap();
        for id in all {
            relay.store().verify(&id).unwrap();
        }
    }
}

/// Number of object files in a repository's store.
pub fn object_count(repo: &Repo) -> usize {
    repo.store().list().unwrap().ids.len()
}

pub fn verify(repo: &Repo) {
    let r = concurv_workspace::verify(repo, true);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
}
