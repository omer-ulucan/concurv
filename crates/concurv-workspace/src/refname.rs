//! Branch-name rules (ADR 0007 §5).
//!
//! Three layers, all applied when a branch is **created** (names already in a
//! stored view are only checked by the format rule of RFC 0001):
//!
//! 1. [`git_refname_ok`]: exactly Git's `check-ref-format` rules for
//!    `refs/heads/<name>`, so every concurv branch can be exported to Git
//!    under the same name (verified against `git` by a differential test).
//! 2. [`portable`]: concurv's stricter policy. Git stores branches as files
//!    on Windows and macOS, where some valid ref names cannot be created or
//!    silently alias each other. ASCII only (no Unicode normalization
//!    aliases), no characters Windows refuses in file names, no Windows
//!    device names as components, at most 200 bytes, nothing that starts
//!    with `-`, no `HEAD`, and the `refs`, `remote` and `remotes` first
//!    components are reserved (the last two for remote-tracking references,
//!    RFC 0003 §5).
//! 3. [`namespace_conflict`]: no collision with an existing branch: equal
//!    ignoring ASCII case, or one being a `/`-path prefix of the other (Git
//!    cannot hold `a` and `a/b` at once), also ignoring case.

use concurv_core::{ConcurvError, ErrorCode, Result};

/// Longest branch name accepted for new branches.
pub const MAX_BRANCH_NAME: usize = 200;

/// Components reserved as the first part of a branch name.
pub const RESERVED_FIRST_COMPONENTS: &[&str] = &["refs", "remote", "remotes"];

const WINDOWS_DEVICES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM0", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT0", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    "CONIN$", "CONOUT$",
];

fn invalid(name: &str, why: impl std::fmt::Display) -> ConcurvError {
    ConcurvError::new(
        ErrorCode::InvalidArgument,
        format!("invalid branch name {name:?}: {why}"),
    )
}

/// Git's rules for `refs/heads/<name>` (`git check-ref-format`), as a
/// reason for the first violated rule, or `None` if Git accepts it.
pub fn git_refname_violation(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("empty");
    }
    if name.contains("@{") {
        return Some("`@{` is not allowed");
    }
    if name.contains("..") {
        return Some("`..` is not allowed");
    }
    for c in name.chars() {
        if (c as u32) < 0x20 || c == '\x7f' {
            return Some("control characters are not allowed");
        }
        if matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\') {
            return Some("space and `~ ^ : ? * [ \\` are not allowed");
        }
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return Some("empty `/`-separated part");
    }
    if name.ends_with('.') {
        return Some("must not end with `.`");
    }
    for part in name.split('/') {
        if part.starts_with('.') {
            return Some("a `/`-separated part must not start with `.`");
        }
        if part.ends_with(".lock") {
            return Some("a `/`-separated part must not end with `.lock`");
        }
    }
    None
}

/// True if Git accepts `refs/heads/<name>`.
pub fn git_refname_ok(name: &str) -> bool {
    git_refname_violation(name).is_none()
}

/// concurv's portability policy on top of Git's rules.
pub fn portable(name: &str) -> Result<()> {
    if let Some(why) = git_refname_violation(name) {
        return Err(invalid(name, why));
    }
    if name.len() > MAX_BRANCH_NAME {
        return Err(invalid(
            name,
            format_args!("longer than {MAX_BRANCH_NAME} bytes"),
        ));
    }
    if !name.is_ascii() {
        return Err(invalid(
            name,
            "only ASCII is accepted (non-ASCII names can alias each other under Unicode \
             normalization when Git stores them as files)",
        ));
    }
    if name.contains(['<', '>', '"', '|']) {
        return Err(invalid(
            name,
            "`< > \" |` cannot appear in file names on Windows, where Git stores branches as files",
        ));
    }
    if name.starts_with('-') {
        return Err(invalid(name, "must not start with `-`"));
    }
    if name == "HEAD" || name == "@" {
        // Git expands `@` to HEAD wherever a branch name is expected.
        return Err(invalid(name, "`HEAD` and `@` are reserved"));
    }
    let parts: Vec<&str> = name.split('/').collect();
    if let Some(first) = parts.first()
        && RESERVED_FIRST_COMPONENTS
            .iter()
            .any(|r| r.eq_ignore_ascii_case(first))
    {
        return Err(invalid(
            name,
            format_args!("the first part `{first}` is reserved (refs / remote-tracking names)"),
        ));
    }
    for part in &parts {
        let stem = part.split('.').next().unwrap_or(part).trim_end();
        if WINDOWS_DEVICES.iter().any(|d| d.eq_ignore_ascii_case(stem)) {
            return Err(invalid(
                name,
                format_args!("`{part}` is a Windows device name"),
            ));
        }
        if part.ends_with(' ') {
            return Err(invalid(name, "a part must not end with a space"));
        }
    }
    Ok(())
}

/// The existing branch that `name` would collide with, if any.
pub fn namespace_conflict<'a>(
    name: &str,
    existing: impl IntoIterator<Item = &'a String>,
) -> Option<&'a String> {
    existing.into_iter().find(|e| collides(name, e))
}

/// True if `a` and `b` cannot both exist: equal, one a `/`-prefix of the other
/// (Git cannot store `x` and `x/y` side by side), or a shared leading part
/// that differs only in ASCII case (`Topic/y` vs `topic/x`: on a
/// case-insensitive filesystem Git keeps both under one directory, and a
/// branch name can come back with different case).
fn collides(a: &str, b: &str) -> bool {
    let (pa, pb): (Vec<&str>, Vec<&str>) = (a.split('/').collect(), b.split('/').collect());
    for (x, y) in pa.iter().zip(&pb) {
        if x != y {
            return x.eq_ignore_ascii_case(y);
        }
    }
    // One is equal to, or a `/`-prefix of, the other.
    true
}

/// All rules for a new branch name given the existing ones.
pub fn check_new(name: &str, existing: &[&String]) -> Result<()> {
    portable(name)?;
    if let Some(e) = namespace_conflict(name, existing.iter().copied()) {
        let msg = if e == name {
            format!("a branch named `{name}` already exists; nothing was changed")
        } else {
            format!(
                "branch name `{name}` collides with existing branch `{e}` (same name ignoring \
                 case, or one is a `/` prefix of the other, which Git cannot store side by side); \
                 nothing was changed"
            )
        };
        return Err(ConcurvError::new(ErrorCode::InvalidArgument, msg));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_rules() {
        for ok in [
            "main",
            "feature/login",
            "v1.2",
            "a-b_c",
            "a@b",
            "x/y/z",
            "ü",
            "a{b}",
            "a!b",
            "a#b",
            "a.b/c.d",
            "lock",
            "a.locks",
        ] {
            assert!(git_refname_ok(ok), "{ok}");
        }
        for bad in [
            "",
            "a@{b",
            "a..b",
            "a b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "/a",
            "a/",
            "a//b",
            "a.",
            ".a",
            "a/.b",
            "a.lock",
            "a/b.lock/c",
            "a\tb",
            "a\x7fb",
        ] {
            assert!(!git_refname_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn portability_policy() {
        for ok in ["main", "feature/login", "release-1.0", "agent/7/fix_x"] {
            portable(ok).unwrap();
        }
        for bad in [
            "ü",
            "a<b",
            "a|b",
            "a\"b",
            "-x",
            "HEAD",
            "@",
            "refs/x",
            "Remote/x",
            "remotes",
            "con",
            "a/NUL.txt",
            "lpt1/x",
            &"x".repeat(201),
        ] {
            assert!(portable(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn namespace_collisions() {
        let existing: Vec<String> = ["main", "feature/a", "Release"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let refs: Vec<&String> = existing.iter().collect();
        for bad in [
            "main",
            "MAIN",
            "feature",
            "Feature",
            "feature/a",
            "feature/A/b",
            "release",
            "main/x",
        ] {
            assert!(check_new(bad, &refs).is_err(), "{bad}");
        }
        for ok in ["feature/b", "featurex", "mainline", "releases"] {
            check_new(ok, &refs).unwrap();
        }
        // A leading part that differs only in case collides.
        assert!(check_new("FEATURE/z", &refs).is_err());
        assert!(check_new("Main/x", &refs).is_err());
    }
}
