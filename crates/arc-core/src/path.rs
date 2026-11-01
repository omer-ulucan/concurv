//! Path and file-name rules (RFC 0001 §4).

use std::fmt;
use std::path::{Path, PathBuf};

use crate::error::{ArcError, ErrorCode, Result};

/// Maximum length of one tree entry name in bytes.
pub const MAX_NAME_LEN: usize = 255;

/// Name of the metadata directory; rejected as a tree entry at every level.
pub const METADATA_DIR: &str = ".arc";

/// Validate one tree-entry name independent of platform.
pub fn validate_name(name: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(ArcError::new(
            ErrorCode::InvalidPath,
            format!("invalid file name {name:?}: {why}"),
        ))
    };
    if name.is_empty() {
        return bad("empty name");
    }
    if name.len() > MAX_NAME_LEN {
        return bad("longer than 255 bytes");
    }
    if name == "." || name == ".." {
        return bad("`.` and `..` are not allowed");
    }
    if name.contains(['/', '\\', '\0']) {
        return bad("contains `/`, `\\` or NUL");
    }
    if name.eq_ignore_ascii_case(METADATA_DIR) {
        return bad("`.arc` is reserved for repository metadata");
    }
    Ok(())
}

/// Platform restrictions applied before materializing files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformRules {
    /// Enforce Windows reserved names and characters.
    pub windows_names: bool,
    /// Refuse sibling names that collide under case folding.
    pub case_insensitive: bool,
}

impl PlatformRules {
    /// Rules for the platform this binary was compiled for.
    pub const fn current() -> Self {
        Self {
            windows_names: cfg!(windows),
            case_insensitive: cfg!(any(windows, target_os = "macos")),
        }
    }

    /// No extra restrictions (Linux).
    pub const fn permissive() -> Self {
        Self {
            windows_names: false,
            case_insensitive: false,
        }
    }

    /// Check one name against these platform rules.
    pub fn check_name(&self, name: &str) -> Result<()> {
        if self.windows_names {
            check_windows_name(name)?;
        }
        Ok(())
    }

    /// Check one directory's set of entry names for case-fold collisions.
    pub fn check_siblings<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Result<()> {
        if !self.case_insensitive {
            return Ok(());
        }
        let mut seen = std::collections::HashMap::new();
        for name in names {
            let folded = name.to_lowercase();
            if let Some(prev) = seen.insert(folded, name) {
                return Err(ArcError::new(
                    ErrorCode::InvalidPath,
                    format!(
                        "names {prev:?} and {name:?} collide on this case-insensitive platform"
                    ),
                ));
            }
        }
        Ok(())
    }
}

const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "COM0", "COM1", "COM2", "COM3", "COM4",
    "COM5", "COM6", "COM7", "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9", "COM¹", "COM²", "COM³", "LPT¹", "LPT²", "LPT³",
];

fn check_windows_name(name: &str) -> Result<()> {
    let bad = |why: &str| {
        Err(ArcError::new(
            ErrorCode::InvalidPath,
            format!("file name {name:?} cannot be created safely on Windows: {why}"),
        ))
    };
    if name
        .chars()
        .any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32)
    {
        return bad("contains a reserved character");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return bad("ends with a dot or space");
    }
    // NTFS 8.3 short names such as `ARC~1` may alias `.arc` (cf. Git's `GIT~1`).
    let upper = name.to_ascii_uppercase();
    if let Some(rest) = upper.strip_prefix("ARC~")
        && !rest.is_empty()
        && rest.bytes().all(|b| b.is_ascii_digit())
    {
        return bad("may alias the `.arc` metadata directory via an NTFS short name");
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    if WINDOWS_RESERVED
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem) || r.to_uppercase() == stem.to_uppercase())
    {
        return bad("reserved device name");
    }
    Ok(())
}

/// A validated, relative repository path made of validated names.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RepoPath(Vec<String>);

impl RepoPath {
    /// The repository root (empty path).
    pub fn root() -> Self {
        Self(Vec::new())
    }

    /// Parse a `/`-separated path, validating each component.
    pub fn parse(s: &str) -> Result<Self> {
        if s.is_empty() {
            return Ok(Self::root());
        }
        let mut parts = Vec::new();
        for part in s.split('/') {
            validate_name(part)?;
            parts.push(part.to_owned());
        }
        Ok(Self(parts))
    }

    /// Append a validated component.
    pub fn join(&self, name: &str) -> Result<Self> {
        validate_name(name)?;
        let mut v = self.0.clone();
        v.push(name.to_owned());
        Ok(Self(v))
    }

    /// Components.
    pub fn components(&self) -> &[String] {
        &self.0
    }

    /// True for the root path.
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Native path below `root`. Safe because every component was validated.
    pub fn to_native(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for c in &self.0 {
            p.push(c);
        }
        p
    }
}

impl fmt::Display for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("/"))
    }
}

impl fmt::Debug for RepoPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RepoPath({:?})", self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_separators() {
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a\0b",
            ".arc",
            ".ARC",
            &"x".repeat(256),
        ] {
            assert!(validate_name(bad).is_err(), "{bad:?} accepted");
        }
        for good in ["a", ".arcignore", "..a", "naïve.txt", " x", "a b"] {
            validate_name(good).unwrap();
        }
        assert!(RepoPath::parse("a/../b").is_err());
        assert!(RepoPath::parse("/abs").is_err());
        assert_eq!(RepoPath::parse("a/b").unwrap().to_string(), "a/b");
    }

    #[test]
    fn windows_rules() {
        let w = PlatformRules {
            windows_names: true,
            case_insensitive: true,
        };
        for bad in [
            "CON", "con.txt", "aux.c", "Nul", "COM1.log", "a:b", "x.", "y ", "q?", "ARC~1",
            "arc~12",
        ] {
            assert!(w.check_name(bad).is_err(), "{bad:?} accepted");
        }
        for good in ["console", "conx.txt", "COM10", "file.txt"] {
            w.check_name(good).unwrap();
        }
        assert!(w.check_siblings(["README", "readme"]).is_err());
        w.check_siblings(["a", "b"]).unwrap();
        PlatformRules::permissive()
            .check_siblings(["README", "readme"])
            .unwrap();
    }
}
