//! Running `git` as a subprocess, hermetically.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use concurv_core::{ConcurvError, ErrorCode, Result};

/// A Git repository addressed by path (bare or with a working tree).
#[derive(Clone, Debug)]
pub struct GitRepo {
    path: PathBuf,
}

/// Environment variables that would redirect `git` away from `path`.
const SCRUBBED_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
];

fn tool_error(what: impl Into<String>) -> ConcurvError {
    ConcurvError::new(ErrorCode::ExternalTool, what)
}

impl GitRepo {
    /// Open the Git repository at `path`; fails if `git` is missing or
    /// `path` is not a Git repository.
    pub fn open(path: &Path) -> Result<Self> {
        let repo = Self {
            path: path.to_path_buf(),
        };
        let out = repo.run(["rev-parse", "--git-dir"])?;
        if !out.status.success() {
            return Err(ConcurvError::new(
                ErrorCode::InvalidArgument,
                format!(
                    "`{}` is not a Git repository ({})",
                    path.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            ));
        }
        Ok(repo)
    }

    /// Repository path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A `git -C <repo>` command with a scrubbed environment.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut c = Command::new("git");
        c.arg("-C").arg(&self.path).args(args);
        for v in SCRUBBED_ENV {
            c.env_remove(v);
        }
        c
    }

    /// Run to completion, capturing output. Only a missing `git` is an error
    /// here; callers inspect the exit status.
    pub fn run<I, S>(&self, args: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                tool_error(format!(
                    "cannot run `git` (is it installed and on PATH?): {e}"
                ))
            })
    }

    /// Run and require success; returns trimmed stdout.
    pub fn run_ok<I, S>(&self, args: I, what: &str) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self.run(args)?;
        if !out.status.success() {
            return Err(tool_error(format!(
                "git failed to {what}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Spawn with piped stdin/stdout.
    pub fn spawn<I, S>(&self, args: I) -> Result<Child>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                tool_error(format!(
                    "cannot run `git` (is it installed and on PATH?): {e}"
                ))
            })
    }

    /// Current commit of `refname`, if it exists.
    pub fn resolve(&self, refname: &str) -> Result<Option<String>> {
        let out = self.run([
            "rev-parse",
            "-q",
            "--verify",
            &format!("{refname}^{{commit}}"),
        ])?;
        Ok(out
            .status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()))
    }

    /// Zero object id for this repository's hash (SHA-1 or SHA-256).
    pub fn zero_oid(&self) -> Result<String> {
        let fmt = self.run_ok(
            ["rev-parse", "--show-object-format"],
            "read the object format",
        )?;
        Ok("0".repeat(if fmt == "sha256" { 64 } else { 40 }))
    }

    /// True if `a` is an ancestor of (or equal to) `b`.
    pub fn is_ancestor(&self, a: &str, b: &str) -> Result<bool> {
        let out = self.run(["merge-base", "--is-ancestor", a, b])?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(tool_error(format!(
                "git merge-base failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ))),
        }
    }
}

/// Wait for a child and require success.
pub fn finish(child: Child, what: &str) -> Result<Vec<u8>> {
    let out = child
        .wait_with_output()
        .map_err(|e| tool_error(format!("git {what} failed: {e}")))?;
    if !out.status.success() {
        return Err(tool_error(format!(
            "git {what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}
