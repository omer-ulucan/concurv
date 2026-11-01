# concurv roadmap status

Legend: `not started` · `in progress` · `gate passed` · `blocked`.
A stage is `gate passed` only with the evidence listed next to it.
Last updated: 2026-11-01.

| Stage | Status | Evidence / blockers |
|---|---|---|
| R0 — RFC & competitive reproduction | **in progress** | Done: RFC 0001–0003, ADR 0001–0003, threat model, comparison plan with exact expected behaviour for S1–S10. **Open gate items:** competitor harness (`bench/scenarios/`) not built; **no** Git/jj/but/gitta/Fossil/Pijul scenario reproduced. |
| R1 — Native local VCS kernel | **gate passed** (2026-11-01) | CI run [41350688459](https://github.com/omer-ulucan/concurv/actions/runs/41350688459) on `main` @ `9b8ed23`: all gate checks pass on Linux, macOS and Windows. See evidence below. |
| R2 — Private workspaces + concurrency | **in progress** | Slices 2a and 2b (RFC 0004) implemented and green in CI on Linux, macOS and Windows: workspaces, publish (CAS), update with file-level merge, workspace-scoped undo, 2/5/10 concurrent writers, killed processes. **Open:** 2c conflict objects and resolution, 2d minimum Git import/export. Gate not passed until those land. See "R2 evidence" below. |
| R3 — Two-PC authenticated sync | not started | design only: RFC 0003 |
| R4 — Verified integration | not started | — |
| R5 — Agent-aware coordination | not started | — |
| R6 — Developer beta + Git ecosystem | not started | design only: ADR 0002 |
| R7 — Advanced VCS | not started | — |

## R1 evidence

CI run 41350688459, commit `9b8ed23` on `main`, rustc 1.99.1 (stable), all
with `--locked`:

| Job | fmt | clippy `-D warnings` | tests (`--no-fail-fast`) | release build |
|---|---|---|---|---|
| ubuntu-latest (x86_64 Linux) | pass | pass | **61 passed, 0 failed, 0 ignored** | pass |
| macos-latest (arm64, APFS) | pass | pass | **61 passed, 0 failed, 0 ignored** | pass |
| windows-latest (x86_64, NTFS) | pass | pass | **60 passed, 0 failed, 0 ignored** | pass |
| MSRV: `cargo check` on Rust 1.89 (Linux) | — | — | — | pass |

Counts differ by platform only through platform-specific tests: 4 Unix-only
tests (exec bits and symlinks, FIFO, non-UTF-8 names, symlink-escape restore)
and 3 Windows-only tests (junctions, unpaired-surrogate names, reserved names).
The same suite also passes locally on Windows 11 Home 10.0.26300 (rustc 1.95.0).

Gate item → tests:

| R1 gate requirement | Tests | Platforms |
|---|---|---|
| Property-based snapshot→restore round-trip of random trees | `concurv-workspace/tests/roundtrip_props.rs` (48 cases/run), `concurv-objects/tests/objects.rs` proptests (encoding canonicality, decoder totality, bit-flip) | all 3 |
| Fault injection at each transaction boundary never exposes a ref to missing objects | `concurv-workspace/tests/fault_injection.rs`: save ≈72 cases (6 failpoints × every hit, incl. torn writes; the exact count varies with fan-out directory creation), init 38 cases; `restore_safety.rs::interrupted_restore_is_reported_and_resumable` fails every rename of a restore | all 3 |
| Corrupt objects detected, never accepted | `objects.rs::corruption_is_detected_never_accepted`, `oversized_headers_*`, `engine.rs::corrupt_reachable_object_*`, `missing_snapshot_member_*`, `restore_safety.rs::corrupt_blob_in_target_*`, CLI exit 30/31 | all 3 |
| Deterministic tree hashing across OS fixture suite | `objects.rs::golden_hash_vectors`, `engine.rs::deterministic_tree_ids_across_repositories` (pinned ids, RFC 0001 §8) | **identical ids on Linux, macOS, Windows** |
| Safe restore (dirty, traversal, symlink escape, encodings) | `restore_safety.rs` (incl. Unix symlink escape), Windows names/junction/surrogate tests | all 3 (platform tests where applicable) |
| CLI end-to-end | `concurv-cli/tests/cli_smoke.rs` (incl. `op log`) | all 3 |

What the gate does **not** prove (still true, by design of the tests):

- Fault injection simulates a process dying at each software write step; it is
  not a power-loss test and cannot catch hardware or filesystems that ignore flushes.
- On Windows, directory fsync is not available (RFC 0002 §5).
- macOS refuses non-UTF-8 names at the filesystem level, so concurv's own
  rejection of them is exercised on Linux only.
- Fuzzing is not configured. No benchmarks have been run, so no performance numbers exist.

## R1 known gaps (do not block the gate, tracked for follow-up)

- No ignore file; every `status`/`save` reads all files (no stat cache).
- The project was renamed ARC → concurv on 2026-11-01 (ADR 0003).

## Next

R2 slices 2a and 2b are done (below); slices 2c and 2d are next. Open R0 work (competitor
reproduction harness) continues in parallel and does not block R2.

## R2 evidence (slice 2a; the stage gate is not yet claimed)

CI run [41354312895](https://github.com/omer-ulucan/concurv/actions/runs/41354312895)
on branch `feat/r2-workspaces`: Linux 81, macOS 81, Windows 80 tests, all
passed, 0 failed, 0 ignored (rustc 1.99.1); MSRV 1.89 check passed.

| R2 gate requirement (roadmap §9) | Tests | Status |
|---|---|---|
| 2, 5 and 10 simultaneous local writers preserve all acknowledged changes, with retries and conflicting target-head updates | `concurv-workspace/tests/concurrency.rs` (`two_writers`, `five_writers`, `ten_writers`): every acked save is in `main`'s history; `main` holds every writer's final content; the op log is linear; `verify --full` passes. Locally 15/15 consecutive runs green after the fix below | passing |
| … even during forced termination; reopening after a crash needs no manual object surgery | `concurv-cli/tests/kill_recovery.rs`: 12 rounds, 4 workspaces, real `concurv` processes killed at random points in save/update/publish (≈40 kills per run); `verify --full` after every round; recovery uses only the documented `restore --force` | passing |
| N private workspaces, one logical branch, no shared index | `concurv-workspace/tests/workspaces.rs` (isolation, nested workspaces refused, names) | passing |
| Atomic CAS publication | `diverged_publish_is_refused_and_changes_nothing`, `save_does_not_move_main_and_publish_fast_forwards`, CLI exit 42 | passing |
| Conflict handling | file-level merge laws (proptest in `merge.rs`), same-file / delete-modify / add-add / file-directory conflicts refused, criss-cross merge base refused (`workspaces.rs`), CLI exit 43 | refused safely; **stored conflict objects and resolution: slice 2c, not started** |
| Workspace-specific op log and per-workspace undo | `concurv-workspace/tests/undo.rs` (save, update, restore and restore --force undo; multi-level; publish refused; other workspaces untouched), concurrent save+undo in every round of `concurrency.rs`, CLI `undo_command` | passing (slice 2b) |
| Git import/export minimum subset | — | **slice 2d, not started** |

A real data-integrity bug was found by these tests and fixed before merge: the
primary workspace's temp cleanup ran on the store's `tmp/` under the wrong
lock, deleting other writers' in-flight temp files. On Windows this could
leave `op_head` naming a vanished operation object (RFC 0004 §5). Before the
fix, at least one concurrency test failed in 4 of 6 runs; after it (plus making
cleanup races best effort), 15/15 consecutive runs passed.
