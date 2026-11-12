//! `concurv check` (RFC 0005 §5): run a branch policy's checks on a
//! candidate snapshot and record an attestation.
//!
//! - **Opt-in per policy version.** Checks are repository code. They run
//!   only if this device approved the policy's exact blob id
//!   (`.concurv/trusted-policies`, local), else `CONCURV_UNTRUSTED_POLICY`.
//! - **No TOCTOU.** The candidate is written from the store into a fresh
//!   directory (every object verified); the working copy is never used, so
//!   what was checked is exactly what would be published.
//! - **Explicit sandbox.** Policy version 1 states `"sandbox": "none"`:
//!   checks run as the user, without isolation, with a cleared environment
//!   (an allow-list plus the names the policy lists) and a timeout.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use concurv_core::{AttestationId, BlobId, ConcurvError, ErrorCode, RandomId, Result, SnapshotId};
use concurv_objects::encoding::{EXIT_NOT_STARTED, EXIT_TIMEOUT};
use concurv_objects::fs::write_file_atomic;
use concurv_objects::{Attestation, CheckRecord, EntryKind};

use crate::policy::{self, POLICY_FILE, Policy, satisfies};
use crate::repo::Repo;

/// Environment variables every check receives (if set).
const BASE_ENV: [&str; 16] = [
    "PATH",
    "HOME",
    "USER",
    "USERNAME",
    "USERPROFILE",
    "LANG",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
];

/// Options for [`check`].
#[derive(Clone, Debug, Default)]
pub struct CheckOptions {
    /// Branch whose policy applies (default: the tracked branch).
    pub branch: Option<String>,
    /// Snapshot to check (default: this workspace's head).
    pub candidate: Option<SnapshotId>,
    /// Approve the governing policy on this device first.
    pub trust: bool,
}

/// Result of [`check`].
#[derive(Clone, Debug)]
pub struct CheckOutcome {
    /// The attestation recorded (pass or fail).
    pub attestation: AttestationId,
    /// Its content.
    pub record: Attestation,
    /// True if it passed.
    pub passed: bool,
    /// The policy was approved by this call.
    pub newly_trusted: bool,
}

fn trusted_path(repo: &Repo) -> PathBuf {
    repo.store_dir().join("trusted-policies")
}

fn index_path(repo: &Repo, candidate: SnapshotId) -> PathBuf {
    repo.store_dir()
        .join("attestations")
        .join(candidate.to_hex())
}

fn read_lines(p: &Path) -> Result<Vec<String>> {
    match fs::read_to_string(p) {
        Ok(s) => Ok(s.lines().map(str::to_owned).collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(ConcurvError::io(e, "read", p)),
    }
}

/// True if this device approved policy `id`.
pub fn is_trusted(repo: &Repo, id: BlobId) -> Result<bool> {
    Ok(read_lines(&trusted_path(repo))?
        .iter()
        .any(|l| l == &id.to_hex()))
}

/// Approve policy `id` on this device (the policy's checks may then run).
pub fn trust(repo: &Repo, id: BlobId) -> Result<bool> {
    let _lock = repo.lock()?;
    let p = trusted_path(repo);
    let mut lines = read_lines(&p)?;
    if lines.iter().any(|l| l == &id.to_hex()) {
        return Ok(false);
    }
    lines.push(id.to_hex());
    let body = format!("{}\n", lines.join("\n"));
    write_file_atomic(
        repo.store().tmp_dir(),
        &p,
        body.as_bytes(),
        &repo.options().faults,
    )?;
    Ok(true)
}

/// Attestations recorded for `candidate` on this repository (a cache: each
/// is re-read and verified before use).
pub fn attestations_for(repo: &Repo, candidate: SnapshotId) -> Result<Vec<AttestationId>> {
    let mut out = Vec::new();
    for l in read_lines(&index_path(repo, candidate))? {
        if let Ok(id) = concurv_core::ObjectId::from_hex(&l) {
            out.push(AttestationId::from_object_id(id));
        }
    }
    Ok(out)
}

/// A passing attestation that authorizes `candidate` on `branch` under the
/// governing policy, if one is recorded. Corrupt, stale or failing ones are
/// skipped; the reasons are returned for the error message.
pub fn find_authorizing(
    repo: &Repo,
    candidate: SnapshotId,
    branch: &str,
    policy_id: BlobId,
    policy: &Policy,
) -> Result<(Option<AttestationId>, Vec<String>)> {
    let mut reasons = Vec::new();
    for id in attestations_for(repo, candidate)?.into_iter().rev() {
        match repo.store().read_attestation(&id) {
            Ok(a) => match satisfies(&a, candidate, branch, policy_id, policy) {
                Ok(()) => return Ok((Some(id), reasons)),
                Err(why) => reasons.push(format!("{}: {why}", &id.to_hex()[..12])),
            },
            Err(e) => reasons.push(format!("{}: {}", &id.to_hex()[..12], e.message)),
        }
    }
    Ok((None, reasons))
}

/// Write `candidate`'s files into the empty directory `dir`, verifying
/// every object.
fn materialize(repo: &Repo, candidate: SnapshotId, dir: &Path) -> Result<()> {
    for (path, state) in repo.snapshot_files(&candidate)? {
        let dest = path.to_native(dir);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| ConcurvError::io(e, "create", parent))?;
        }
        match state.kind {
            EntryKind::Symlink => {
                let target = repo.store().read_blob(&state.blob)?;
                let target = String::from_utf8(target).map_err(|_| {
                    ConcurvError::new(ErrorCode::ObjectCorrupt, "symlink target is not UTF-8")
                })?;
                make_symlink(&target, &dest)?;
            }
            _ => {
                let mut f =
                    fs::File::create(&dest).map_err(|e| ConcurvError::io(e, "create", &dest))?;
                repo.store().read_blob_to(&state.blob, &mut f)?;
                f.flush().map_err(|e| ConcurvError::io(e, "write", &dest))?;
                #[cfg(unix)]
                if state.kind == EntryKind::Executable {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))
                        .map_err(|e| ConcurvError::io(e, "chmod", &dest))?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_symlink(target: &str, dest: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, dest).map_err(|e| ConcurvError::io(e, "symlink", dest))
}

#[cfg(not(unix))]
fn make_symlink(_target: &str, dest: &Path) -> Result<()> {
    Err(ConcurvError::new(
        ErrorCode::UnsupportedFileType,
        "the candidate contains a symlink, which cannot be checked out on this platform",
    )
    .with_path(dest))
}

/// Run one check; returns (exit code, duration) and appends output to `out`.
fn run_one(
    argv: &[String],
    dir: &Path,
    env: &[(String, String)],
    timeout: Duration,
    out: &Path,
) -> Result<(i64, u64)> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .map_err(|e| ConcurvError::io(e, "create", out))?;
    let log2 = log
        .try_clone()
        .map_err(|e| ConcurvError::io(e, "open", out))?;
    let start = Instant::now();
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(dir)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log2));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(out)
                .map_err(|e| ConcurvError::io(e, "open", out))?;
            let _ = writeln!(f, "concurv: cannot start {:?}: {e}", argv[0]);
            return Ok((EXIT_NOT_STARTED, 0));
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                return Ok((status.code().map_or(-1, i64::from), ms));
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
                return Ok((EXIT_TIMEOUT, ms));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(ConcurvError::io(e, "wait for", Path::new(&argv[0]))),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Run the governing policy's checks on the candidate and record an
/// attestation (pass or fail). Publishes nothing.
pub fn check(repo: &Repo, opts: &CheckOptions) -> Result<CheckOutcome> {
    let (_, op, ws) = repo.current()?;
    let branch = match &opts.branch {
        Some(b) => b.clone(),
        None => ws.branch.clone().ok_or_else(|| {
            ConcurvError::new(
                ErrorCode::InvalidArgument,
                "this workspace tracks no branch; pass --branch",
            )
        })?,
    };
    let candidate = match opts.candidate.or(ws.head) {
        Some(c) => c,
        None => {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                "nothing to check: this workspace has no saved snapshot",
            ));
        }
    };
    let head = op.view.branches.get(&branch).copied().ok_or_else(|| {
        ConcurvError::new(
            ErrorCode::NotFound,
            format!("branch {branch} has no head, so no policy governs it"),
        )
    })?;
    let Some((policy_id, policy)) = policy::load(repo.store(), head)? else {
        return Err(ConcurvError::new(
            ErrorCode::NotFound,
            format!("the head of {branch} has no {POLICY_FILE}: there is nothing to check"),
        ));
    };
    let mut newly_trusted = false;
    if !is_trusted(repo, policy_id)? {
        if !opts.trust {
            let cmds: Vec<String> = policy
                .checks
                .iter()
                .map(|c| format!("  {}: {}", c.name, c.argv.join(" ")))
                .collect();
            return Err(ConcurvError::new(
                ErrorCode::UntrustedPolicy,
                format!(
                    "the policy of {branch} ({}) has not been approved on this device; its checks run \
                     repository commands without isolation:\n{}\nReview them, then run `concurv check --trust`",
                    &policy_id.to_hex()[..12],
                    cmds.join("\n")
                ),
            ));
        }
        newly_trusted = trust(repo, policy_id)?;
    }
    if !policy.allows_this_os() {
        return Err(ConcurvError::new(
            ErrorCode::PolicyFail,
            format!(
                "the policy of {branch} only accepts attestations made on {}; this is {}",
                policy.environment.os.join(", "),
                std::env::consts::OS
            ),
        ));
    }

    let rid = RandomId::generate()?.to_hex();
    let dir = std::env::temp_dir().join(format!("concurv-check-{rid}"));
    fs::create_dir(&dir).map_err(|e| ConcurvError::io(e, "create", &dir))?;
    let result = run_checks(repo, candidate, &policy, &dir);
    let _ = fs::remove_dir_all(&dir);
    let (started_ms, checks) = result?;
    let record = Attestation {
        candidate,
        policy: policy_id,
        branch: branch.clone(),
        device_id: repo.device_id(),
        started_ms,
        finished_ms: now_ms(),
        environment: serde_json::json!({
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "concurv": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
        checks,
    };
    // The store gains a new object kind: repository format 3 first.
    {
        let lock = repo.lock()?;
        repo.upgrade_format(&lock, 3)?;
    }
    let attestation = repo.store().write_attestation(&record)?;
    {
        let _lock = repo.lock()?;
        let p = index_path(repo, candidate);
        if let Some(parent) = p.parent() {
            concurv_objects::fs::create_dir_durable(parent, &repo.options().faults)?;
        }
        let mut lines = read_lines(&p)?;
        if !lines.contains(&attestation.to_hex()) {
            lines.push(attestation.to_hex());
        }
        let body = format!("{}\n", lines.join("\n"));
        write_file_atomic(
            repo.store().tmp_dir(),
            &p,
            body.as_bytes(),
            &repo.options().faults,
        )?;
    }
    Ok(CheckOutcome {
        attestation,
        passed: record.passed(),
        record,
        newly_trusted,
    })
}

fn run_checks(
    repo: &Repo,
    candidate: SnapshotId,
    policy: &Policy,
    dir: &Path,
) -> Result<(i64, Vec<CheckRecord>)> {
    let tree = dir.join("tree");
    fs::create_dir(&tree).map_err(|e| ConcurvError::io(e, "create", &tree))?;
    materialize(repo, candidate, &tree)?;
    let names: BTreeSet<&str> = BASE_ENV
        .iter()
        .copied()
        .chain(policy.env.iter().map(String::as_str))
        .collect();
    let env: Vec<(String, String)> = names
        .into_iter()
        .filter_map(|n| std::env::var(n).ok().map(|v| (n.to_owned(), v)))
        .collect();
    let started_ms = now_ms();
    let writer = repo.store_writer()?;
    let mut records = Vec::new();
    for (i, c) in policy.checks.iter().enumerate() {
        let out = dir.join(format!("output-{i}"));
        fs::write(&out, b"").map_err(|e| ConcurvError::io(e, "create", &out))?;
        let (exit_code, duration_ms) =
            run_one(&c.argv, &tree, &env, Duration::from_secs(c.timeout_s), &out)?;
        let len = fs::metadata(&out)
            .map_err(|e| ConcurvError::io(e, "inspect", &out))?
            .len();
        let f = fs::File::open(&out).map_err(|e| ConcurvError::io(e, "open", &out))?;
        let output = writer.store().write_blob_stream(f, len, &out)?;
        records.push(CheckRecord {
            name: c.name.clone(),
            argv: c.argv.clone(),
            exit_code,
            duration_ms,
            output,
        });
    }
    Ok((started_ms, records))
}
