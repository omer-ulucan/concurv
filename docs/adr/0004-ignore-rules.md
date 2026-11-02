# ADR 0004 — Ignore rules and the tracked-file model

Status: **Accepted, implemented** (R1.5). Date: 2026-11-01.

## Context

Until R1.5 the scanner versioned every file except `.concurv/`. Build outputs
(`target/`, `node_modules/`) and local files such as `.env` were saved, and so
was the `.git/` directory of a workspace that is also a Git working tree.

## Decision

1. **Syntax and semantics are Git's**: `.gitignore` files in any directory,
   plus a workspace-local `.concurv/exclude` (the analogue of
   `.git/info/exclude`, never versioned). No new syntax and no
   `.concurvignore`.
2. **Precedence, as in Git**: the deepest `.gitignore` first, then shallower
   ones, then `.concurv/exclude`; the first source with a matching pattern
   decides; within one file the last matching line wins; `!` re-includes. A
   path inside an excluded directory is excluded and cannot be re-included.
   An excluded directory is not entered.
3. **Tracked means "present in the workspace head"**. Tracked paths are always
   scanned, even if an ignore rule matches them (a modification is
   `modified`, a removal is `deleted`). An excluded directory is still entered
   when it contains tracked files, but untracked files inside it stay
   excluded. This needs no persisted index and no format change; explicit
   `track`/`untrack` is a separate future item (P1-6).
4. **Ignored files are never overwritten or deleted.** `restore`, `update`,
   `undo` and the checkout of `workspace add` refuse (exit 20, nothing
   modified) when the target would write over an ignored, untracked file or
   need a directory where one sits. This also applies with `--force`, because
   the `--force` backup cannot contain ignored files. Directories are pruned
   only when empty, so ignored files keep their directories. A file that
   already holds exactly the target content is not a collision (this lets an
   interrupted restore resume).
5. **`.git` is never scanned**, at any depth. Untracked directories that are
   themselves repositories (they contain `.git` or `.concurv`) are not
   scanned, as in Git. On every platform, restore refuses to materialize a
   `.git` entry (plus `GIT~n` on Windows), so a snapshot cannot plant hooks or
   config into a colocated Git repository. This is a restore-time policy, not
   a format rule, so older snapshots stay readable.
6. **Matching engine**: the `ignore` crate's gitignore matcher, adapted where
   differential testing against real Git showed differences:
   - `{` and `}` are escaped (Git has no brace expansion);
   - a pattern with an unclosed `[` never matches (Git's matcher aborts on it);
   - patterns the engine cannot compile (for example `[z-a]` or a trailing `\`)
     never match and are reported by `check-ignore`. **Known divergence:** for
     a reversed range like `[z-a]`, Git matches the range's first character.
     concurv matches nothing, so it versions more, never less.
   Git never rejects an ignore file, so neither does concurv: `status` and
   `save` never fail because of a pattern.
7. **Case-sensitive on every platform**, so the same tree gives the same
   decisions everywhere. (Git on Windows/macOS defaults to
   `core.ignorecase=true`.)
8. Ignore rules are **not a secrets boundary**. They do not remove anything
   already saved; history redaction is a separate future need.

## Evidence

- `crates/concurv-workspace/tests/ignore.rs`: semantics, tracked-stays-tracked,
  local exclude, restore/update/undo safety, resume, `check-ignore`, Unix FIFO
  and symlinked `.gitignore`.
- `crates/concurv-workspace/tests/ignore_vs_git.rs`: **differential test
  against real Git** (`git ls-files --others --ignored --exclude-standard`) over
  7 corpus cases; concurv must ignore exactly the files Git ignores.
- `crates/concurv-workspace/tests/restore_safety.rs::restore_refuses_to_write_into_dot_git`.
- `crates/concurv-cli/tests/cli_smoke.rs::check_ignore_command`.
