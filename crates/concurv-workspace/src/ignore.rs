//! Git-compatible ignore rules for the native scanner (R1.5, ADR 0004:
//! `docs/adr/0004-ignore-rules.md`).
//!
//! Sources, from highest to lowest precedence:
//! 1. `.gitignore` files in the working copy, the deepest directory first
//!    (patterns are relative to the directory containing the file; within one
//!    file the last matching pattern wins, as in Git);
//! 2. the workspace-local, never-versioned `.concurv/exclude` (like Git's
//!    `.git/info/exclude`).
//!
//! The first source with a matching pattern decides (`!pattern` re-includes).
//! A path inside an excluded directory is excluded; as in Git, it cannot be
//! re-included. Matching is case-sensitive on every platform, so the same
//! tree gives the same result everywhere.
//!
//! Ignore rules decide only which **untracked** paths are considered. A path
//! present in the workspace head is tracked and is always scanned (see
//! `worktree::scan`). Ignore patterns are not a security boundary for secrets.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ::ignore::Match;
use ::ignore::gitignore::{Gitignore, GitignoreBuilder};
use concurv_core::{ConcurvError, ErrorCode, RepoPath, Result};

/// Name of the per-directory ignore file.
pub const GITIGNORE: &str = ".gitignore";
/// Workspace-local exclude file inside `.concurv/`.
pub const LOCAL_EXCLUDE: &str = "exclude";

/// Where a decisive pattern came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleSource {
    /// A `.gitignore` in this directory of the working copy.
    GitIgnore(RepoPath),
    /// `.concurv/exclude`.
    LocalExclude,
}

impl std::fmt::Display for RuleSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GitIgnore(dir) if dir.is_root() => f.write_str(GITIGNORE),
            Self::GitIgnore(dir) => write!(f, "{dir}/{GITIGNORE}"),
            Self::LocalExclude => write!(f, ".concurv/{LOCAL_EXCLUDE}"),
        }
    }
}

/// Outcome of matching one path against the rules (ignoring parents).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No pattern matched.
    Unmatched,
    /// Excluded by `pattern`.
    Ignored {
        /// Deciding file.
        source: RuleSource,
        /// The pattern as written.
        pattern: String,
    },
    /// Re-included by a `!pattern`.
    Included {
        /// Deciding file.
        source: RuleSource,
        /// The pattern as written (with `!`).
        pattern: String,
    },
}

impl Verdict {
    /// True for [`Verdict::Ignored`].
    pub fn is_ignored(&self) -> bool {
        matches!(self, Self::Ignored { .. })
    }
}

/// A pattern line that cannot be compiled. Git never rejects an ignore
/// pattern (an unusable one simply never matches), so concurv skips the line
/// the same way instead of failing `status`/`save`, and reports it through
/// `check-ignore`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnusablePattern {
    /// File containing the line.
    pub source: RuleSource,
    /// 1-based line number.
    pub line: usize,
    /// The line as written.
    pub pattern: String,
    /// Why it cannot be used.
    pub reason: String,
}

/// Lazily loaded ignore rules for one working copy.
pub struct IgnoreRules {
    root: PathBuf,
    exclude: Option<Gitignore>,
    dirs: HashMap<RepoPath, Option<Gitignore>>,
    unusable: Vec<UnusablePattern>,
}

/// Escape `{` and `}`: Git has no brace expansion, so they are literal
/// characters in Git ignore patterns, while the glob engine would treat them
/// as alternation. Characters already escaped with `\` are left alone.
fn escape_braces(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut escaped = false;
    for c in line.chars() {
        if escaped {
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            out.push(c);
            escaped = true;
        } else if c == '{' || c == '}' {
            out.push('\\');
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

/// True if the pattern has a `[` that opens a character class without a
/// closing `]` (a `]` right after `[` or `[!`/`[^` is a member, not the end).
fn has_unclosed_class(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' => i += 2,
            '[' => {
                let mut j = i + 1;
                if matches!(chars.get(j), Some('!' | '^')) {
                    j += 1;
                }
                if chars.get(j) == Some(&']') {
                    j += 1;
                }
                while j < chars.len() && chars[j] != ']' {
                    if chars[j] == '\\' {
                        j += 1;
                    }
                    j += 1;
                }
                if j >= chars.len() {
                    return true;
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    false
}

/// Parse an ignore file. Only regular files are read: a symlinked or
/// directory-typed `.gitignore` is not followed (it is still versioned
/// normally if tracked). Unusable lines are collected, not fatal.
fn load_file(
    file: &Path,
    base: &Path,
    source: RuleSource,
    unusable: &mut Vec<UnusablePattern>,
) -> Result<Option<Gitignore>> {
    match fs::symlink_metadata(file) {
        Ok(m) if m.is_file() => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConcurvError::io(e, "inspect", file)),
    }
    let bytes = fs::read(file).map_err(|e| ConcurvError::io(e, "read", file))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut b = GitignoreBuilder::new(base);
    for (n, line) in text.lines().enumerate() {
        if has_unclosed_class(line) {
            // Git's matcher aborts on an unclosed `[`: the pattern never
            // matches. (The glob engine would treat it as a literal `[`.)
            unusable.push(UnusablePattern {
                source: source.clone(),
                line: n + 1,
                pattern: line.to_owned(),
                reason: "unclosed `[` character class (never matches, as in Git)".to_owned(),
            });
            continue;
        }
        if let Err(e) = b.add_line(Some(file.to_path_buf()), &escape_braces(line)) {
            unusable.push(UnusablePattern {
                source: source.clone(),
                line: n + 1,
                pattern: line.to_owned(),
                reason: e.to_string(),
            });
        }
    }
    let g = b.build().map_err(|e| {
        ConcurvError::new(
            ErrorCode::Internal,
            format!("cannot build ignore rules from `{}`: {e}", file.display()),
        )
    })?;
    Ok((!g.is_empty()).then_some(g))
}

impl IgnoreRules {
    /// Rules for the working copy at `root`. Reads `.concurv/exclude` now and
    /// each directory's `.gitignore` on first use.
    pub fn load(root: &Path) -> Result<Self> {
        let mut unusable = Vec::new();
        let exclude = load_file(
            &root.join(".concurv").join(LOCAL_EXCLUDE),
            root,
            RuleSource::LocalExclude,
            &mut unusable,
        )?;
        Ok(Self {
            root: root.to_path_buf(),
            exclude,
            dirs: HashMap::new(),
            unusable,
        })
    }

    /// Pattern lines skipped so far because they cannot be used.
    pub fn unusable(&self) -> &[UnusablePattern] {
        &self.unusable
    }

    fn dir_rules(&mut self, dir: &RepoPath) -> Result<Option<&Gitignore>> {
        if !self.dirs.contains_key(dir) {
            let base = dir.to_native(&self.root);
            let g = load_file(
                &base.join(GITIGNORE),
                &base,
                RuleSource::GitIgnore(dir.clone()),
                &mut self.unusable,
            )?;
            self.dirs.insert(dir.clone(), g);
        }
        Ok(self.dirs.get(dir).and_then(Option::as_ref))
    }

    /// Match `path` (relative, not the root) against every source, deepest
    /// `.gitignore` first. Does not consider whether a parent is excluded.
    pub fn verdict(&mut self, path: &RepoPath, is_dir: bool) -> Result<Verdict> {
        let native = path.to_native(&self.root);
        let comps = path.components();
        for depth in (0..comps.len()).rev() {
            let dir = RepoPath::parse(&comps[..depth].join("/"))?;
            let Some(g) = self.dir_rules(&dir)? else {
                continue;
            };
            if let Some(v) = to_verdict(g.matched(&native, is_dir), RuleSource::GitIgnore(dir)) {
                return Ok(v);
            }
        }
        if let Some(g) = &self.exclude
            && let Some(v) = to_verdict(g.matched(&native, is_dir), RuleSource::LocalExclude)
        {
            return Ok(v);
        }
        Ok(Verdict::Unmatched)
    }

    /// Full decision including excluded parents: returns the first excluded
    /// ancestor directory's verdict, or the path's own verdict.
    pub fn explain(
        &mut self,
        path: &RepoPath,
        is_dir: bool,
    ) -> Result<(Verdict, Option<RepoPath>)> {
        let comps = path.components();
        for depth in 1..comps.len() {
            let dir = RepoPath::parse(&comps[..depth].join("/"))?;
            let v = self.verdict(&dir, true)?;
            if v.is_ignored() {
                return Ok((v, Some(dir)));
            }
        }
        Ok((self.verdict(path, is_dir)?, None))
    }
}

fn to_verdict(m: Match<&::ignore::gitignore::Glob>, source: RuleSource) -> Option<Verdict> {
    match m {
        Match::None => None,
        Match::Ignore(g) => Some(Verdict::Ignored {
            source,
            pattern: g.original().to_owned(),
        }),
        Match::Whitelist(g) => Some(Verdict::Included {
            source,
            pattern: g.original().to_owned(),
        }),
    }
}

/// What `check-ignore` reports for one path.
#[derive(Clone, Debug)]
pub struct IgnoreReport {
    /// Workspace-relative path.
    pub path: RepoPath,
    /// True if the path is a directory on disk (or was given as `dir/`).
    pub is_dir: bool,
    /// True if the path (or, for a directory, something inside it) is in the
    /// workspace head. Tracked paths are scanned whatever the rules say.
    pub tracked: bool,
    /// True if the scanner skips this path: untracked and excluded.
    pub ignored: bool,
    /// The deciding rule: for an excluded parent, that parent's rule.
    pub verdict: Verdict,
    /// An excluded ancestor directory, if that is what decides.
    pub excluded_parent: Option<RepoPath>,
}

/// Explain how the scanner treats each path. Read-only. Also returns every
/// pattern line skipped as unusable in the rules files consulted.
pub fn check_ignore(
    root: &Path,
    head: &crate::worktree::FlatTree,
    paths: &[(RepoPath, bool)],
) -> Result<(Vec<IgnoreReport>, Vec<UnusablePattern>)> {
    let mut rules = IgnoreRules::load(root)?;
    let mut out = Vec::with_capacity(paths.len());
    for (path, dir_hint) in paths {
        let is_dir =
            *dir_hint || fs::symlink_metadata(path.to_native(root)).is_ok_and(|m| m.is_dir());
        let tracked = if is_dir {
            head.keys()
                .any(|k| k.components().starts_with(path.components()) && k != path)
        } else {
            head.contains_key(path)
        };
        let (verdict, excluded_parent) = rules.explain(path, is_dir)?;
        let excluded = excluded_parent.is_some() || verdict.is_ignored();
        out.push(IgnoreReport {
            path: path.clone(),
            is_dir,
            tracked,
            ignored: excluded && !tracked,
            verdict,
            excluded_parent,
        });
    }
    let unusable = rules.unusable().to_vec();
    Ok((out, unusable))
}
