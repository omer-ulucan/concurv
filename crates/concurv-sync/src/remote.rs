//! Remotes of a repository: where a relay is, how to recognize it, and the
//! device token to present (RFC 0003 §3.1).
//!
//! Configuration, not history: `.concurv/remotes/<name>/config` holds the
//! URL and the pinned certificate fingerprint; `.concurv/remotes/<name>/token`
//! holds the device token (owner-only permissions on Unix). The environment
//! variable `CONCURV_REMOTE_TOKEN` overrides the stored token. Remote-tracking
//! references are history and live in the operation view (`View::remotes`).

use std::fs;
use std::path::{Path, PathBuf};

use concurv_core::{ConcurvError, ErrorCode, OperationId, RandomId, Result};
use concurv_objects::Operation;
use concurv_objects::encoding::validate_remote_name;
use concurv_objects::fs::{create_dir_durable, rename_durable, write_file_atomic};
use concurv_workspace::Repo;

use crate::client::RelayUrl;
use crate::tls::parse_fingerprint;

const CONFIG_HEADER: &str = "concurv-remote 1";
/// Environment variable that overrides a remote's stored token.
pub const TOKEN_ENV: &str = "CONCURV_REMOTE_TOKEN";

/// A configured remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteConfig {
    /// Name.
    pub name: String,
    /// Relay address.
    pub url: RelayUrl,
    /// Pinned certificate fingerprint (`sha256:…`).
    pub fingerprint: String,
}

fn remotes_dir(repo: &Repo) -> PathBuf {
    repo.store_dir().join("remotes")
}

fn not_found(name: &str) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::NotFound,
        format!("no remote named {name:?} (see `concurv remote list`)"),
    )
}

fn check_token(token: &str) -> Result<()> {
    let ok = token.len() <= 256 && !token.is_empty() && token.bytes().all(|b| b.is_ascii_graphic());
    if ok {
        Ok(())
    } else {
        Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            "the device token must be printable ASCII without spaces (as printed by `concurvd device add`)",
        ))
    }
}

#[cfg(unix)]
fn write_secret(tmp: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let t = tmp.join(format!("token.{}.tmp", RandomId::generate()?.to_hex()));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&t)
        .map_err(|e| ConcurvError::io(e, "create", &t))?;
    let r = f.write_all(bytes).and_then(|()| f.sync_all());
    drop(f);
    if let Err(e) = r {
        let _ = fs::remove_file(&t);
        return Err(ConcurvError::io(e, "write", &t));
    }
    rename_durable(&t, dest, &concurv_objects::Faults::none())
}

#[cfg(not(unix))]
fn write_secret(tmp: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    // Windows: the file inherits the ACL of the user's repository directory.
    write_file_atomic(tmp, dest, bytes, &concurv_objects::Faults::none())
}

/// Add a remote. Upgrades the repository to format 2 (remote-tracking
/// references) if needed. Contacts nothing.
pub fn add(
    repo: &Repo,
    name: &str,
    url: &str,
    fingerprint: &str,
    token: &str,
) -> Result<RemoteConfig> {
    validate_remote_name(name)?;
    let url = RelayUrl::parse(url)?;
    parse_fingerprint(fingerprint)?;
    check_token(token)?;
    let lock = repo.lock()?;
    let dir = remotes_dir(repo);
    let rdir = dir.join(name);
    if rdir.exists() {
        return Err(ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!("a remote named {name:?} already exists"),
        ));
    }
    repo.upgrade_for_remotes(&lock)?;
    let faults = repo.options().faults.clone();
    create_dir_durable(&dir, &faults)?;
    // Build the remote in a staging directory, then rename it into place.
    let staging = dir.join(format!(".new-{}", RandomId::generate()?.to_hex()));
    create_dir_durable(&staging, &faults)?;
    let tmp = repo.store().tmp_dir();
    let cfg = format!("{CONFIG_HEADER}\nurl {url}\nfingerprint {fingerprint}\n");
    write_file_atomic(tmp, &staging.join("config"), cfg.as_bytes(), &faults)?;
    write_secret(tmp, &staging.join("token"), format!("{token}\n").as_bytes())?;
    rename_durable(&staging, &rdir, &faults)?;
    drop(lock);
    Ok(RemoteConfig {
        name: name.to_owned(),
        url,
        fingerprint: fingerprint.to_owned(),
    })
}

fn parse_config(name: &str, path: &Path) -> Result<RemoteConfig> {
    let s = fs::read_to_string(path).map_err(|e| ConcurvError::io(e, "read", path))?;
    let bad = || {
        ConcurvError::new(
            ErrorCode::ObjectCorrupt,
            format!("remote {name:?} has a malformed configuration"),
        )
        .with_path(path)
    };
    let mut lines = s.lines();
    if lines.next() != Some(CONFIG_HEADER) {
        return Err(bad());
    }
    let url = lines
        .next()
        .and_then(|l| l.strip_prefix("url "))
        .ok_or_else(bad)?;
    let fp = lines
        .next()
        .and_then(|l| l.strip_prefix("fingerprint "))
        .ok_or_else(bad)?;
    if lines.next().is_some() {
        return Err(bad());
    }
    parse_fingerprint(fp).map_err(|_| bad())?;
    Ok(RemoteConfig {
        name: name.to_owned(),
        url: RelayUrl::parse(url).map_err(|_| bad())?,
        fingerprint: fp.to_owned(),
    })
}

/// One remote's configuration.
pub fn get(repo: &Repo, name: &str) -> Result<RemoteConfig> {
    validate_remote_name(name).map_err(|_| not_found(name))?;
    let p = remotes_dir(repo).join(name).join("config");
    if !p.exists() {
        return Err(not_found(name));
    }
    parse_config(name, &p)
}

/// All remotes, sorted by name.
pub fn list(repo: &Repo) -> Result<Vec<RemoteConfig>> {
    let dir = remotes_dir(repo);
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(ConcurvError::io(e, "list", &dir)),
    };
    let mut out = Vec::new();
    for e in rd {
        let e = e.map_err(|e| ConcurvError::io(e, "list", &dir))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if validate_remote_name(&name).is_ok() && e.path().join("config").exists() {
            out.push(parse_config(&name, &e.path().join("config"))?);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The token to present to remote `name`.
pub fn token(repo: &Repo, name: &str) -> Result<String> {
    if let Ok(t) = std::env::var(TOKEN_ENV)
        && !t.is_empty()
    {
        check_token(&t)?;
        return Ok(t);
    }
    get(repo, name)?;
    let p = remotes_dir(repo).join(name).join("token");
    let t = fs::read_to_string(&p).map_err(|e| ConcurvError::io(e, "read", &p))?;
    let t = t.trim_end_matches('\n').to_owned();
    check_token(&t)?;
    Ok(t)
}

/// Remove a remote: its configuration and token, and, in one operation, its
/// remote-tracking references. Local branches are not touched, so no local
/// work becomes unreachable (ADR 0007 B1). Returns the operation, if the
/// view had references to drop.
pub fn remove(repo: &Repo, name: &str) -> Result<Option<OperationId>> {
    get(repo, name)?;
    let lock = repo.lock()?;
    let (op_id, op) = repo.oplog().head()?;
    let mut operation = None;
    if op.view.remotes.contains_key(name) {
        let mut view = op.view.clone();
        view.remotes.remove(name);
        let new_op = Operation {
            parents: vec![op_id],
            timestamp_ms: repo.now_ms(),
            device_id: repo.device_id(),
            workspace_id: None,
            kind: "remote-remove".into(),
            description: format!("remove remote {name} and its remote-tracking references"),
            backup: None,
            undoes: None,
            checked_out: None,
            view,
        };
        operation = Some(repo.oplog().commit(&lock, &op_id, &new_op)?);
    }
    let rdir = remotes_dir(repo).join(name);
    let gone = remotes_dir(repo).join(format!(".old-{}", RandomId::generate()?.to_hex()));
    rename_durable(&rdir, &gone, &repo.options().faults)?;
    let _ = fs::remove_dir_all(&gone);
    Ok(operation)
}
