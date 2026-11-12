//! Configuration files (R6): a **repository** file, `.concurv/config.json`
//! in the repository's metadata (shared by all its workspaces), and a
//! **user** file. A closed set of keys: an unknown key or a malformed file is
//! an error, never silently ignored.
//!
//! The user file is `$CONCURV_CONFIG` when that variable is set (an empty
//! value means "no user file"); otherwise `%APPDATA%\concurv\config.json` on
//! Windows and `$XDG_CONFIG_HOME/concurv/config.json` (default
//! `~/.config/concurv/config.json`) elsewhere.
//!
//! Precedence, highest first: the command line, the environment
//! (`CONCURV_AUTHOR`), the repository file, the user file, built-in
//! defaults. Configuration never changes history, the byte format or what a
//! command is allowed to do; it only supplies defaults.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use concurv_core::{ConcurvError, ErrorCode, Result};
use concurv_objects::Faults;
use concurv_objects::encoding::validate_remote_name;
use concurv_objects::fs::{FileLock, write_file_atomic};
use serde::{Deserialize, Serialize};

use crate::Repo;

/// Environment variable naming the user configuration file.
pub const CONFIG_ENV: &str = "CONCURV_CONFIG";
/// File name of the repository configuration inside `.concurv/`.
pub const REPO_CONFIG_FILE: &str = "config.json";
/// Largest configuration file read.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// The known keys and what they set.
pub const KEYS: &[(&str, &str)] = &[
    (
        "author",
        "author recorded in snapshots and operations, e.g. `Ada Lovelace <ada@example.com>` (overridden by CONCURV_AUTHOR)",
    ),
    ("remote", "remote used by fetch and push when none is named"),
];

/// Which file a value comes from or goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `.concurv/config.json` of the repository.
    Repo,
    /// The user's file (see the module documentation).
    User,
}

impl Scope {
    /// Name used in output.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Repo => "repository",
            Scope::User => "user",
        }
    }
}

/// On-disk form: `{"concurv_config": 1, "settings": {"key": "value"}}`.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct File {
    concurv_config: u32,
    #[serde(default)]
    settings: BTreeMap<String, String>,
}

/// One configuration file's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config {
    settings: BTreeMap<String, String>,
}

impl Config {
    /// Value of `key`, if set.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.settings.get(key).map(String::as_str)
    }

    /// All settings, sorted by key.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.settings.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Read `path`; a missing file is an empty configuration.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = match fs::metadata(path) {
            Ok(m) if m.len() > MAX_CONFIG_BYTES => {
                return Err(invalid(path, "larger than 64 KiB"));
            }
            Ok(_) => fs::read(path).map_err(|e| ConcurvError::io(e, "read", path))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConcurvError::io(e, "read", path)),
        };
        let file: File =
            serde_json::from_slice(&bytes).map_err(|e| invalid(path, &e.to_string()))?;
        if file.concurv_config != 1 {
            return Err(invalid(
                path,
                &format!("unsupported version {}", file.concurv_config),
            ));
        }
        for (k, v) in &file.settings {
            validate(k, v).map_err(|e| invalid(path, &e.message))?;
        }
        Ok(Self {
            settings: file.settings,
        })
    }

    fn encode(&self) -> Result<Vec<u8>> {
        let mut body = serde_json::to_vec_pretty(&File {
            concurv_config: 1,
            settings: self.settings.clone(),
        })
        .map_err(|e| ConcurvError::internal(e.to_string()))?;
        body.push(b'\n');
        Ok(body)
    }
}

fn invalid(path: &Path, why: &str) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::InvalidArgument,
        format!(
            "invalid configuration file `{}`: {why} (fix or remove it)",
            path.display()
        ),
    )
    .with_path(path)
}

/// Check that `key` is known and `value` acceptable for it.
pub fn validate(key: &str, value: &str) -> Result<()> {
    let bad = |why: &str| {
        ConcurvError::new(
            ErrorCode::InvalidArgument,
            format!("invalid value for `{key}`: {why}"),
        )
    };
    match key {
        "author" => {
            if value.trim().is_empty() || value.trim() != value {
                return Err(bad("must be non-empty, without leading or trailing spaces"));
            }
            if value.len() > 256 || value.chars().any(char::is_control) {
                return Err(bad("at most 256 bytes, no control characters"));
            }
            Ok(())
        }
        "remote" => validate_remote_name(value).map_err(|e| bad(&e.message)),
        _ => Err(unknown_key(key)),
    }
}

fn unknown_key(key: &str) -> ConcurvError {
    let known: Vec<&str> = KEYS.iter().map(|(k, _)| *k).collect();
    ConcurvError::new(
        ErrorCode::InvalidArgument,
        format!(
            "unknown configuration key `{key}` (known: {})",
            known.join(", ")
        ),
    )
}

/// The user configuration file, if there is one to use.
pub fn user_path() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(CONFIG_ENV) {
        return (!v.is_empty()).then(|| PathBuf::from(v));
    }
    let nonempty = |var: &str| std::env::var_os(var).filter(|v| !v.is_empty());
    let base = if cfg!(windows) {
        nonempty("APPDATA").map(PathBuf::from)?
    } else if let Some(x) = nonempty("XDG_CONFIG_HOME") {
        PathBuf::from(x)
    } else {
        PathBuf::from(nonempty("HOME")?).join(".config")
    };
    Some(base.join("concurv").join("config.json"))
}

/// The repository configuration file of `repo`.
pub fn repo_path(repo: &Repo) -> PathBuf {
    repo.store_dir().join(REPO_CONFIG_FILE)
}

/// A value and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    /// The value.
    pub value: String,
    /// `"repository"`, `"user"` or `"environment"`.
    pub source: &'static str,
}

/// The configuration that applies in `repo` (or outside any repository):
/// both files, loaded and validated.
#[derive(Clone, Debug, Default)]
pub struct Layers {
    /// Repository file (empty outside a repository).
    pub repo: Config,
    /// User file.
    pub user: Config,
}

impl Layers {
    /// Load the files that apply to `repo` (`None`: outside a repository).
    pub fn load(repo: Option<&Repo>) -> Result<Self> {
        let repo_cfg = match repo {
            Some(r) => Config::load(&repo_path(r))?,
            None => Config::default(),
        };
        let user = match user_path() {
            Some(p) => Config::load(&p)?,
            None => Config::default(),
        };
        Ok(Self {
            repo: repo_cfg,
            user,
        })
    }

    /// Effective value of `key` (environment, then repository, then user).
    pub fn get(&self, key: &str) -> Result<Option<Effective>> {
        if !KEYS.iter().any(|(k, _)| *k == key) {
            return Err(unknown_key(key));
        }
        if key == "author"
            && let Ok(v) = std::env::var("CONCURV_AUTHOR")
            && !v.trim().is_empty()
        {
            return Ok(Some(Effective {
                value: v.trim().to_owned(),
                source: "environment",
            }));
        }
        let found = self
            .repo
            .get(key)
            .map(|v| (v, Scope::Repo))
            .or_else(|| self.user.get(key).map(|v| (v, Scope::User)));
        Ok(found.map(|(v, s)| Effective {
            value: v.to_owned(),
            source: s.as_str(),
        }))
    }
}

/// Set (`Some`) or remove (`None`) `key` in the file of `scope`, atomically
/// and under a lock (the repository lock for the repository file). Returns
/// the previous value.
pub fn write(
    repo: Option<&Repo>,
    scope: Scope,
    key: &str,
    value: Option<&str>,
) -> Result<Option<String>> {
    match value {
        Some(v) => validate(key, v)?,
        None => {
            if !KEYS.iter().any(|(k, _)| *k == key) {
                return Err(unknown_key(key));
            }
        }
    }
    let apply = |path: &Path, tmp: &Path, faults: &Faults| -> Result<Option<String>> {
        let mut cfg = Config::load(path)?;
        let old = match value {
            Some(v) => cfg.settings.insert(key.to_owned(), v.to_owned()),
            None => cfg.settings.remove(key),
        };
        if old.as_deref() != value {
            write_file_atomic(tmp, path, &cfg.encode()?, faults)?;
        }
        Ok(old)
    };
    match scope {
        Scope::Repo => {
            let repo = repo.ok_or_else(|| {
                ConcurvError::new(
                    ErrorCode::NotARepo,
                    "not inside a concurv repository (use --global for the user configuration)",
                )
            })?;
            let _lock = repo.lock()?;
            apply(
                &repo_path(repo),
                repo.store().tmp_dir(),
                &repo.options().faults,
            )
        }
        Scope::User => {
            let path = user_path().ok_or_else(|| {
                ConcurvError::new(
                    ErrorCode::NotFound,
                    format!("no user configuration directory (set {CONFIG_ENV}, or HOME/APPDATA)"),
                )
            })?;
            let dir = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            fs::create_dir_all(&dir).map_err(|e| ConcurvError::io(e, "create directory", &dir))?;
            let _lock = FileLock::acquire(
                &dir.join("config.lock"),
                Duration::from_secs(30),
                "the user configuration",
            )?;
            apply(&path, &dir, &Faults::none())
        }
    }
}
