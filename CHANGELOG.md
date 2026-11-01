# Changelog

All notable changes to concurv. Nothing has been released yet. Format
version 1 is not frozen until the first release (see ADR 0003).

## [Unreleased]

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
- No command lists the operation log, so backup ids are only visible in
  `restore` output.
- No ignore file, stat cache, GC, rename detection, multiple workspaces, undo,
  merge, sync or Git bridge.
- Tested on Windows only; the Linux and macOS runs are outstanding.
