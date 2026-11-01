# Changelog

All notable changes to concurv. Nothing has been released yet. Format
version 1 is not frozen until the first release (see ADR 0003).

## [Unreleased]

R1 gate passed on 2026-11-01: the full suite is green on Linux, macOS and
Windows in CI, with identical golden hashes on all three (`docs/ROADMAP_STATUS.md`).

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
- No ignore file, stat cache, GC, rename detection, multiple workspaces, undo,
  merge, sync or Git bridge.
