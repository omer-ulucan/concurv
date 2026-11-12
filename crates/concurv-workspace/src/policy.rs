//! Branch policies and attestation binding (RFC 0005, R4).
//!
//! A policy is the file [`POLICY_FILE`] at the root of a branch's **current
//! head**, never of the candidate: a candidate cannot change the rules it
//! is judged by. A branch the policy lists in `protect` accepts a new head
//! only with a passing attestation bound to exactly that candidate, that
//! policy and that branch ([`satisfies`]).

use concurv_core::{BlobId, ConcurvError, ErrorCode, Result, SnapshotId};
use concurv_objects::encoding::{MAX_ARGS, MAX_CHECKS, validate_branch_name};
use concurv_objects::{Attestation, EntryKind, ObjectStore};
use serde::Deserialize;

/// Name of the policy file at the root of a tree.
pub const POLICY_FILE: &str = "concurv-policy.json";
/// Largest policy file accepted.
pub const MAX_POLICY_BYTES: usize = 64 * 1024;
/// Longest timeout a check may declare (one day).
pub const MAX_TIMEOUT_S: u64 = 86_400;

/// A parsed, validated policy (`"concurv_policy": 1`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Policy format version; must be 1.
    pub concurv_policy: u32,
    /// Branches that require a passing attestation.
    #[serde(default)]
    pub protect: Vec<String>,
    /// Allowed environments.
    #[serde(default)]
    pub environment: Environment,
    /// Checks, run in order.
    pub checks: Vec<Check>,
    /// Extra environment variables passed to checks (names).
    #[serde(default)]
    pub env: Vec<String>,
    /// Isolation of checks. Version 1 knows only `"none"`: checks run as
    /// the user, without isolation, in a fresh directory.
    pub sandbox: String,
}

/// Environment constraints.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// Operating systems allowed to produce attestations (`std::env::consts::OS`
    /// names: `linux`, `macos`, `windows`, …). Empty: any.
    #[serde(default)]
    pub os: Vec<String>,
}

/// One check.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Unique name.
    pub name: String,
    /// Program and arguments (no shell).
    pub argv: Vec<String>,
    /// Seconds before the check is stopped and counted as failed.
    pub timeout_s: u64,
}

fn invalid(why: impl std::fmt::Display) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::PolicyFail,
        format!("invalid {POLICY_FILE}: {why}"),
    )
}

fn env_name_ok(n: &str) -> bool {
    let mut b = n.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
        && n.len() <= 128
}

impl Policy {
    /// Parse and validate. Unknown fields, versions or sandboxes are errors
    /// (`CONCURV_POLICY_FAIL`), never ignored.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_POLICY_BYTES {
            return Err(invalid(format!("larger than {MAX_POLICY_BYTES} bytes")));
        }
        let p: Policy = serde_json::from_slice(bytes).map_err(invalid)?;
        if p.concurv_policy != 1 {
            return Err(invalid(format!(
                "policy version {} is not supported by this build",
                p.concurv_policy
            )));
        }
        if p.sandbox != "none" {
            return Err(invalid(format!(
                "sandbox {:?} is not supported by this build (only \"none\")",
                p.sandbox
            )));
        }
        for b in &p.protect {
            validate_branch_name(b).map_err(|e| invalid(e.message))?;
        }
        if p.checks.is_empty() || p.checks.len() > MAX_CHECKS as usize {
            return Err(invalid(format!("needs 1 to {MAX_CHECKS} checks")));
        }
        let mut names = std::collections::HashSet::new();
        for c in &p.checks {
            if c.name.is_empty()
                || c.name.len() > 64
                || !c.name.bytes().all(|b| (b' '..=b'~').contains(&b))
                || c.name.trim() != c.name
                || !names.insert(c.name.as_str())
            {
                return Err(invalid(format!(
                    "check name {:?} must be unique, 1-64 printable ASCII characters, no outer spaces",
                    c.name
                )));
            }
            if c.argv.is_empty()
                || c.argv.len() > MAX_ARGS as usize
                || c.argv[0].is_empty()
                || c.argv.iter().any(|a| a.contains('\0') || a.len() > 4096)
            {
                return Err(invalid(format!(
                    "check {:?} needs a program and at most {MAX_ARGS} arguments",
                    c.name
                )));
            }
            if c.timeout_s == 0 || c.timeout_s > MAX_TIMEOUT_S {
                return Err(invalid(format!(
                    "check {:?}: timeout_s must be 1 to {MAX_TIMEOUT_S}",
                    c.name
                )));
            }
        }
        if let Some(bad) = p.env.iter().find(|n| !env_name_ok(n)) {
            return Err(invalid(format!(
                "invalid environment variable name {bad:?}"
            )));
        }
        if p.environment
            .os
            .iter()
            .any(|o| o.is_empty() || o.len() > 32)
        {
            return Err(invalid("invalid os name"));
        }
        Ok(p)
    }

    /// True if `branch` is protected.
    pub fn protects(&self, branch: &str) -> bool {
        self.protect.iter().any(|b| b == branch)
    }

    /// True if attestations may be produced on this operating system.
    pub fn allows_this_os(&self) -> bool {
        self.environment.os.is_empty()
            || self
                .environment
                .os
                .iter()
                .any(|o| o == std::env::consts::OS)
    }
}

/// The policy file of `snapshot`, if any: its blob id and parsed content.
/// A policy file that is not a regular file, or does not parse, is an
/// error (fail closed).
pub fn load(store: &ObjectStore, snapshot: SnapshotId) -> Result<Option<(BlobId, Policy)>> {
    let s = store.read_snapshot(&snapshot)?;
    let tree = store.read_tree(&s.tree)?;
    let Some(e) = tree.get(POLICY_FILE) else {
        return Ok(None);
    };
    if !matches!(e.kind, EntryKind::File | EntryKind::Executable) {
        return Err(invalid("not a regular file"));
    }
    let id = BlobId::from_object_id(e.id);
    let bytes = store.read_blob(&id)?;
    Ok(Some((id, Policy::parse(&bytes)?)))
}

/// What publishing to a branch requires.
#[derive(Clone, Debug)]
pub enum Protection {
    /// Nothing beyond the usual fast-forward compare-and-swap.
    None,
    /// A passing attestation under this policy.
    Required {
        /// Blob id of the governing policy file.
        policy_id: BlobId,
        /// The policy.
        policy: Policy,
    },
}

/// The protection of `branch` whose current head is `head` (`None`: the
/// branch does not exist yet, so no policy governs it).
pub fn protection(
    store: &ObjectStore,
    branch: &str,
    head: Option<SnapshotId>,
) -> Result<Protection> {
    let Some(h) = head else {
        return Ok(Protection::None);
    };
    match load(store, h)? {
        Some((policy_id, policy)) if policy.protects(branch) => {
            Ok(Protection::Required { policy_id, policy })
        }
        _ => Ok(Protection::None),
    }
}

/// The operating system recorded in an attestation's environment.
pub fn attested_os(a: &Attestation) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&a.environment).ok()?;
    v.get("os")?.as_str().map(str::to_owned)
}

/// Whether attestation `a` authorizes publishing `candidate` to `branch`
/// under `policy` (blob `policy_id`). `Err` explains why not.
pub fn satisfies(
    a: &Attestation,
    candidate: SnapshotId,
    branch: &str,
    policy_id: BlobId,
    policy: &Policy,
) -> std::result::Result<(), String> {
    if a.candidate != candidate {
        return Err(format!("it attests {}, not the candidate", a.candidate));
    }
    if a.branch != branch {
        return Err(format!("it was made for branch {}", a.branch));
    }
    if a.policy != policy_id {
        return Err("it was made under another version of the policy (stale)".into());
    }
    let same_checks = a.checks.len() == policy.checks.len()
        && a.checks
            .iter()
            .zip(&policy.checks)
            .all(|(r, c)| r.name == c.name && r.argv == c.argv);
    if !same_checks {
        return Err("its checks are not the policy's checks".into());
    }
    if !policy.environment.os.is_empty() {
        let os = attested_os(a).unwrap_or_default();
        if !policy.environment.os.contains(&os) {
            return Err(format!(
                "it was made on {os:?}, which the policy does not allow"
            ));
        }
    }
    if !a.passed() {
        let failed: Vec<&str> = a
            .checks
            .iter()
            .filter(|c| c.exit_code != 0)
            .map(|c| c.name.as_str())
            .collect();
        return Err(format!("checks failed: {}", failed.join(", ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const OK: &str = r#"{"concurv_policy": 1, "protect": ["main"],
        "checks": [{"name": "test", "argv": ["cargo", "test"], "timeout_s": 60}],
        "sandbox": "none"}"#;

    #[test]
    fn valid_policy_parses() {
        let p = Policy::parse(OK.as_bytes()).unwrap();
        assert!(p.protects("main") && !p.protects("dev"));
        assert!(p.allows_this_os());
    }

    #[test]
    fn invalid_policies_fail_closed() {
        let bad = [
            r#"{"concurv_policy": 2, "checks": [{"name":"t","argv":["x"],"timeout_s":1}], "sandbox":"none"}"#,
            r#"{"concurv_policy": 1, "checks": [{"name":"t","argv":["x"],"timeout_s":1}], "sandbox":"docker"}"#,
            r#"{"concurv_policy": 1, "checks": [{"name":"t","argv":["x"],"timeout_s":1}]}"#,
            r#"{"concurv_policy": 1, "checks": [], "sandbox":"none"}"#,
            r#"{"concurv_policy": 1, "checks": [{"name":"t","argv":[],"timeout_s":1}], "sandbox":"none"}"#,
            r#"{"concurv_policy": 1, "checks": [{"name":"t","argv":["x"],"timeout_s":0}], "sandbox":"none"}"#,
            r#"{"concurv_policy": 1, "checks": [{"name":"t","argv":["x"],"timeout_s":1},{"name":"t","argv":["y"],"timeout_s":1}], "sandbox":"none"}"#,
            r#"{"concurv_policy": 1, "checks": [{"name":"t","argv":["x"],"timeout_s":1}], "sandbox":"none", "extra": 1}"#,
            r#"{"concurv_policy": 1, "checks": [{"name":"t","argv":["x"],"timeout_s":1}], "sandbox":"none", "env": ["A=B"]}"#,
            r#"{"concurv_policy": 1, "protect": ["a b"], "checks": [{"name":"t","argv":["x"],"timeout_s":1}], "sandbox":"none"}"#,
            "not json",
        ];
        for b in bad {
            assert_eq!(
                Policy::parse(b.as_bytes()).unwrap_err().code,
                ErrorCode::PolicyFail,
                "{b}"
            );
        }
    }
}
