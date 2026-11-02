# Changelog

All notable changes to concurv. Releases before 0.1.0 are alphas: format
version 1 and the CLI/JSON contract are not frozen until 0.1.0 (see ADR 0003).

## [Unreleased]

### Added (R2.5, ADR 0007)
- **Named branches**: `branch create [--at]`, `branch list`, `branch switch`;
  `workspace add --branch`; `update --from <branch>` merges another branch;
  `git export --from <branch>`. `branch switch` refuses unsaved changes,
  pending merges and saves that would be left on no branch; `undo` reverses
  a switch. New operation kinds `branch-create`, `branch-switch`.

## [0.1.0-alpha.1] — 2026-11-01

First public pre-release (`docs/releases/v0.1.0-alpha.1.md`). No license is
asserted yet.

R1 gate passed on 2026-11-01: the full suite is green on Linux, macOS and
Windows in CI, with identical golden hashes on all three (`docs/ROADMAP_STATUS.md`).
R2 slices 2a (private workspaces), 2b (undo), 2c (conflict resolution) and 2d
(Git bridge) are implemented, and the **R2 gate passed** on 2026-11-01 (CI on
`main`, Linux/macOS/Windows; Git tags are out of scope, see `docs/ROADMAP_STATUS.md`).

### Changed (behaviour)
- **`save` no longer moves `main`** (RFC 0004 §2). It advances only the
  current workspace's head; use `concurv publish` to move `main`.
- Mutating commands **wait** up to 30 s for a busy lock instead of failing
  immediately with `CONCURV_LOCKED`.
- The repository lock is now held only for the commit step; scanning and
  object writing run concurrently with other workspaces.

### Changed (format, pre-release)
- Operation views store a **name** for every workspace (RFC 0001 §3.4,
  RFC 0004 §4). Repositories created by earlier builds of this branch are
  not readable; none were distributed.

### Added (R2 slice 2a, RFC 0004)
- `concurv workspace add <path> [--name N]` and `concurv workspace list`:
  private workspaces that share one store and one `main`.
- `concurv publish`: compare-and-swap fast-forward of `main`;
  `CONCURV_DIVERGED` (exit 42) if `main` has unintegrated work.
- `concurv update`: fast-forward, or a file-level three-way merge into a new
  snapshot in the workspace. Same-file conflicts and ambiguous merge bases
  are refused with `CONCURV_CONFLICT` (exit 43); nothing is modified.
- Stress tests: 2/5/10 concurrent writers; 12 rounds of killed processes.

### Added (R1.5, ADR 0004)
- **Ignore rules like Git**: `.gitignore` (root and nested, negation, anchors,
  `**`, escapes) and a local `.concurv/exclude`. Already-tracked files stay
  tracked. Verified against real Git by a differential test.
- `concurv check-ignore <path>…` explains decisions (`--json`), including
  unusable patterns.
- **Stat cache** (ADR 0005, Unix only): `status`/`diff` skip rehashing files
  whose exact identity (device, inode, size, mode, mtime, ctime) is unchanged
  since the last save; a racy-timestamp guard and a false-clean test suite.
  `status --no-cache` / `diff --no-cache` hash every file. Windows always
  hashes.
- `concurv-bench`: reproducible `status`/`save` benchmark (`bench/RESULTS.md`).
- Golden tests for diff output (` No newline at end of file`, CRLF, binary)
  and a case-collision restore test that runs on every OS.

### Added (R2.5, ADR 0006)
- **Line-level merge in `update`**: text files changed on both sides merge
  when their edits are separated by at least one unchanged line
  (conservative diff3; CRLF and missing final newline kept byte-exactly).
  Binary files, symlinks and overlapping or touching edits stay conflicts.
  `update --json` lists them in `content_merged[]`.

### Changed (R1.5)
- `.git/` directories are never scanned, and untracked nested repositories
  are skipped. History saved by earlier builds that captured `.git/` files
  shows them as deleted.
- `restore`, `update`, `undo` and workspace checkout refuse to overwrite an
  ignored, untracked file, even with `--force` (exit 20), and refuse to write
  a `.git` entry (exit 22).
- License metadata removed: no license is asserted until the owner chooses one.
- `save` hashes a file before storing it and skips the temp write and fsync
  when a verified copy of the object already exists: a no-op save of 10k
  files went from 46.0 s to 1.7 s on the reference machine.

### Added (R2 slice 2d, ADR 0002)
- `concurv git export <repo> [--branch B]` writes `main`'s history (merges
  included) into a Git repository via `git fast-import` and moves the Git
  branch by compare-and-swap; refuses (exit 42) if the Git branch has
  commits outside that history. Deterministic and incremental.
- `concurv git import <repo> [--branch B]` imports a Git branch into a fresh
  repository, publishes it as `main` and checks it out; refuses submodules,
  `.concurv` paths and non-UTF-8 data with a full list (exit 23).
- New crate `concurv-git`; new error code `CONCURV_EXTERNAL_TOOL` (exit 60).

### Added (R2 slice 2c, RFC 0004 §11)
- `concurv update --allow-conflicts` records a pending merge instead of
  refusing: clean changes are applied, conflicting text files get whole-file
  ours/base/theirs markers, binary and delete/modify conflicts keep ours with
  theirs in `<path>.concurv-theirs`, and every conflict gets a
  `<path>.concurv-conflict` note. `save` completes the merge once nothing
  remains (both parents); `update --abort` gives it up (edits backed up).
  `status` shows each conflict as resolved/unresolved; `undo` works.
- Format (pre-release, within v1): conflict object kind 5; workspace view
  entries can reference a pending merge.

### Added (R2 slice 2b, RFC 0004 §10)
- `concurv undo` reverses the most recent operation of the current workspace
  (`save`, `update`, `restore`), exactly what it changed; repeat to go further
  back. Never touches other workspaces or branches; `publish` is refused.
  Undoing `restore --force` brings unsaved work back exactly.
- Operation format (pre-release, within v1): `undoes` and `checked_out`
  fields, also shown in `op log --json`.

### Fixed (found by the new stress tests before release)
- Data integrity: the primary workspace's temp cleanup could delete other
  writers' in-flight temp files, which on Windows could leave `op_head`
  pointing at a vanished operation object. Workspaces now use a separate
  `ws-tmp/`, and each temp root is cleaned only under its own lock.
- Cleanup races with an exiting writer no longer fail commands (best effort).

### Added
- `concurv op log [-n N]` lists the operation log (init/save/restore), newest
  first, with the head after each operation and any backup snapshot recorded
  by `restore --force`. A lost backup id can now be found again.
- GitHub Actions CI: fmt, Clippy, tests and release build on Linux, macOS
  and Windows with the latest stable Rust, plus an MSRV check on Rust 1.89.

### Fixed
- The save/init fault-injection sweeps were flaky (about 1 run in 5): the
  number of directory creations depends on random ids, so a precomputed hit
  count could overshoot. Each case now discovers its own boundaries.
- The non-UTF-8 file-name test now accounts for macOS, whose APFS refuses
  such names at creation (`EILSEQ`).
- Code now passes the Clippy lints added in Rust 1.99 (`as_chunks` for hex
  decoding, a byte-string literal in a test). No behaviour change.

### Changed
- **Project renamed from ARC to concurv** (ADR 0003). Binary `arc` → `concurv`,
  crates `arc-*` → `concurv-*`, metadata directory `.arc/` → `.concurv/`,
  object magic `ARC1` → `CNV1`, hash context → `concurv 2026-11 object v1`,
  error codes `ARC_*` → `CONCURV_*`, JSON field `arc_json_version` →
  `concurv_json_version`, env `ARC_AUTHOR` → `CONCURV_AUTHOR`. Every object id
  changed. Repositories from pre-rename builds are not readable and no
  migration is provided, because none were distributed.

### Added (R1 local kernel)
- `concurv init` creates the repository atomically (staging directory + rename).
- `concurv status` / `concurv diff` are read-only, content-hash based comparisons
  of the working copy with the head.
- `concurv save -m` writes a durable snapshot. Objects are fsynced and renamed,
  then a single atomic swap of `.concurv/op_head` commits the operation.
- `concurv log` shows first-parent snapshot history.
- `concurv restore --snapshot <id> [--force]` verifies the target fully first,
  refuses to discard unsaved work without `--force`, and saves a backup
  snapshot when forced. It never writes through symlinks, and an interrupted
  restore is reported and can be resumed.
- `concurv verify [--full]` checks the integrity of the object graph.
- `--json` output (`concurv_json_version` 1) with stable error and exit codes.
- Canonical BLAKE3 object format (RFC 0001), durability protocol (RFC 0002),
  sync design (RFC 0003, not implemented), threat model.
- 59 tests: golden vectors, property tests, a fault-injection sweep across
  every durable-write boundary, and restore-safety and CLI smoke tests.

### Known gaps
- No stat cache, GC, rename detection, line-level (within-file)
  merging, cross-machine sync, importing Git changes into existing history,
  or Git tags.
