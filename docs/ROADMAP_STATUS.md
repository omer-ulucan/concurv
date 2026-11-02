# concurv roadmap status

Legend: `not started` · `in progress` · `gate passed` · `blocked`.
A stage is `gate passed` only with the evidence listed next to it.
The plan itself (stages, gates, backlog, acceptance tests) lives in
**`docs/ROADMAP_REVISED.md`**. Last updated: 2026-11-01 (R2.5 branch-model refinements).

| Stage | Status | Evidence / blockers |
|---|---|---|
| R0 — RFC & competitive reproduction | **in progress** | Done: RFC 0001–0003, ADR 0001–0003, threat model, comparison plan with exact expected behaviour for S1–S10. **Open gate items:** competitor harness (`bench/scenarios/`) not built; **no** Git/jj/but/gitta/Fossil/Pijul scenario reproduced. |
| R1 — Native local VCS kernel | **gate passed** (2026-11-01) | CI run [41350688459](https://github.com/omer-ulucan/concurv/actions/runs/41350688459) on `main` @ `9b8ed23`: all gate checks pass on Linux, macOS and Windows. See evidence below. |
| R1.5 — Daily VCS usability & safety | **gate passed** (2026-11-01) | CI run [41366633305](https://github.com/omer-ulucan/concurv/actions/runs/41366633305) on `main` @ `19ce0ed`: ignores (ADR 0004), ignored-file safety, `check-ignore`, benchmark, stat cache (ADR 0005), diff and case-collision tests, green on Linux, macOS and Windows. See "R1.5 evidence" below |
| R2 — Private workspaces + concurrency | **gate passed** (2026-11-01) | CI run [41359891403](https://github.com/omer-ulucan/concurv/actions/runs/41359891403) on `main` @ `0689e96`: all four slices (RFC 0004, ADR 0002) green on Linux, macOS and Windows; 2/5/10 concurrent writers and killed-process tests pass. Git tags not supported (scoped). See "R2 evidence" below. |
| R2.5 — Local merge & branch semantics | **in progress** (gate re-opened 2026-11-01) | Line-level merge (ADR 0006) verified. Branch model: the owner approved ADR 0007 **in principle** and requested refinements; they are implemented and tested (see "R2.5 branch-model refinements" below). Remaining gate item: the owner's final acceptance of ADR 0007 |
| R3 — Two-PC authenticated sync | not started | design only: RFC 0003 (draft; must be revised for per-branch reconciliation, ADR 0007, before implementation) |
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
- Fuzzing is not configured. (Benchmarks were added in R1.5: `bench/RESULTS.md`.)

## R1 known gaps (do not block the gate, tracked for follow-up)

- Fixed in R1.5: no ignore file and no stat cache (see "R1.5 evidence").
- The project was renamed ARC → concurv on 2026-11-01 (ADR 0003).

## Next

R0 (harness) remains open; R1, R1.5 and R2 are complete; R2.5 awaits the owner's
final acceptance of ADR 0007. Next: revise
RFC 0003 (per-branch reconciliation, ADR 0007) and start R3 (see `docs/ROADMAP_REVISED.md`). Open R0 work (competitor
reproduction harness) continues in parallel and does not block R3.

## R2 evidence (gate passed 2026-11-01)

CI run [41359891403](https://github.com/omer-ulucan/concurv/actions/runs/41359891403)
on `main` @ `0689e96`, rustc 1.99.1, `--locked`, `--no-fail-fast`:

| Job | Tests |
|---|---|
| ubuntu-latest | **103 passed, 0 failed, 0 ignored** |
| macos-latest (arm64) | **103 passed, 0 failed, 0 ignored** |
| windows-latest | **102 passed, 0 failed, 0 ignored** (no Unix-only tests; has Windows-only ones) |
| MSRV 1.89 `cargo check` | pass |

Gate statement (roadmap §9): *"2, 5 and 10 simultaneous local writers preserve
all acknowledged changes, even during retries, forced termination and
conflicting target head updates. Reopening after crash succeeds without
manual object surgery."*

| Requirement | Tests | Status |
|---|---|---|
| 2, 5 and 10 simultaneous writers keep every acknowledged change, with retries and conflicting target-head updates | `concurv-workspace/tests/concurrency.rs` (`two_writers`, `five_writers`, `ten_writers`): every acked save is in `main`'s history; `main` holds every writer's final content; the op log is linear; scratch save+undo interleaved in every round; `verify --full` passes | **passing** |
| … during forced termination; reopening needs no manual object surgery | `concurv-cli/tests/kill_recovery.rs`: 12 rounds, 4 workspaces, real `concurv` processes killed at random points in save/update/publish (≈40 kills per run); `verify --full` after every round; recovery only via the documented `restore --force` | **passing** |
| N private workspaces on one logical branch; no shared index | `concurv-workspace/tests/workspaces.rs` | **passing** (slice 2a) |
| Workspace-specific heads and op log; per-workspace undo | `concurv-workspace/tests/undo.rs`, CLI `undo_command` | **passing** (slice 2b) |
| Conflict objects | `concurv-workspace/tests/conflicts.rs`, conflict encoding test in `objects.rs`, CLI `conflict_resolution_via_cli` | **passing** (slice 2c) |
| Atomic CAS publication | `diverged_publish_is_refused_and_changes_nothing`, `save_does_not_move_main_and_publish_fast_forwards`, CLI exit 42 | **passing** |
| Git import/export, minimum useful subset: ordinary files, commits, branches | `concurv-git/tests/bridge.rs`: round trip with merge, binary and Unicode paths (identical tree ids, metadata, deterministic re-export to the same commit ids), incremental export, refusal on a diverged Git branch, native Git import, refusal of submodules and `.concurv` paths; CLI `git_export_and_import` | **passing** (slice 2d) |
| … tags "where supported" | concurv has no tag concept yet, so tags are neither imported nor exported (ADR 0002). Recorded as a scoped limitation, not claimed | **not applicable / not implemented** |

Also found and fixed during R2, before merge:

- Data integrity: the primary workspace's temp cleanup ran on the store's
  `tmp/` under the wrong lock, deleting other writers' in-flight temp files;
  on Windows this could leave `op_head` naming a vanished operation object
  (RFC 0004 §5). Before the fix, at least one concurrency test failed in 4 of
  6 runs; after it (plus making cleanup races best effort), 15/15 runs passed.
- Flaky fault-injection enumeration (random ids change the number of
  directory creations); each case now discovers its own boundaries.

What the R2 gate does **not** prove: line-level merging (R4), behaviour across
machines (R3), and power-loss durability beyond the R1 notes above.

## R1.5 evidence (gate passed 2026-11-01)

CI run [41366633305](https://github.com/omer-ulucan/concurv/actions/runs/41366633305)
on `main` @ `19ce0ed`, rustc 1.99.1, `--locked`, `--no-fail-fast`:

| Job | Tests |
|---|---|
| ubuntu-latest | **149 passed, 0 failed, 0 ignored** |
| macos-latest (arm64) | **149 passed, 0 failed, 0 ignored** |
| windows-latest | **145 passed, 0 failed, 0 ignored** |
| MSRV 1.89 `cargo check` | pass |

Backlog ids refer to `docs/ROADMAP_REVISED.md` §3.

| Gate item | Tests / evidence |
|---|---|
| P0-1…P0-3 Git-compatible ignores (nested, negation, anchors, `**`, escapes), local exclude, tracked-stays-tracked | ADR 0004; `tests/ignore.rs`; `tests/ignore_vs_git.rs` compares against real `git ls-files` (7 cases, incl. nested repositories) |
| P0-4 ignored files never overwritten/deleted by restore/update/undo/checkout; no `.git` writes | `ignore.rs` (`restore_never_overwrites_an_ignored_file_even_with_force`, `ignored_files_survive_restore_of_their_directory_and_undo`, `update_refuses_to_overwrite_an_ignored_file_in_another_workspace`), `restore_safety.rs::restore_refuses_to_write_into_dot_git` |
| P0-5 `check-ignore [--json]` | `cli_smoke.rs::check_ignore_command` |
| P0-6 `status` baseline measured | `crates/concurv-bench`, `bench/RESULTS.md`; CI numbers from the `bench` workflow, run [41365713252](https://github.com/omer-ulucan/concurv/actions/runs/41365713252) |
| P0-7 stat cache passing the false-clean suite (same size+mtime, mid-scan change, cache corruption) | ADR 0005; `tests/stat_cache.rs` (12 tests on Unix, 10 on Windows: same-size/same-mtime rewrite, change right after save, repeated edits, rename-over, delete+recreate, corrupt/truncated/missing cache, status never writes, `--no-cache`, two workspaces, symlink retarget + mode change, only settled files cached); unit tests for the racy guard, identity-stability rule and every single-bit flip/truncation of the cache encoding; CLI `status_and_diff_accept_no_cache` |
| P0-8 diff "no newline" and CRLF | `tests/diff_golden.rs` (8 golden cases) |
| P0-9 macOS case-collision test | `restore_safety.rs::case_fold_collisions_follow_the_platform` (refused on macOS/Windows, restored on Linux) |
| CI green on 3 OSes | run above |

Measured effect (CI runners, 10k files / 84.5 MiB, p50): clean `status`
212 → 103 ms on Linux and 350 → 100 ms on macOS with the cache; Windows
always hashes, by design. A no-op `save` of 10k files went from 46.0 s to
1.7 s on the reference Windows machine (hash before write).

What the R1.5 gate does **not** prove: the stat cache on filesystems that do
not maintain `ctime` (ADR 0005 residual risk; use `--no-cache`); ignore
compatibility beyond the tested cases (one known divergence: Git matches a
reversed range `[z-a]`, concurv does not).

## R2.5 evidence, first pass (2026-11-01; branch items superseded below)

CI run [41370038493](https://github.com/omer-ulucan/concurv/actions/runs/41370038493)
on `main` @ `8e450c8`, rustc 1.99.1, `--locked`, `--no-fail-fast`:

| Job | Tests |
|---|---|
| ubuntu-latest | **177 passed, 0 failed, 0 ignored** |
| macos-latest (arm64) | **177 passed, 0 failed, 0 ignored** |
| windows-latest | **173 passed, 0 failed, 0 ignored** |
| MSRV 1.89 `cargo check` | pass |

| Gate item | Tests / evidence |
|---|---|
| Hunk-level 3-way merge: disjoint same-file edits merge, overlap conflicts, reusing the conflict model and existing commands | ADR 0006; `textmerge.rs` unit + property tests (one side unchanged → other side, same change → that change, symmetry); `tests/hunk_merge.rs` (disjoint edits merge and publish, overlap refused with nothing changed, mixed merge records only real conflicts under `--allow-conflicts`, binary never merged, CRLF/no-final-newline byte-exact) |
| Branch-model ADR accepted | First recorded as accepted under the owner's delegation; **superseded**: the owner then approved it in principle only and requested refinements (below) |
| Smallest branch slice (create/list/switch-by-workspace) | `branch create/list/switch`, `workspace add --branch`, `update --from` (since replaced by `merge`), `git export --from`; `tests/branches.rs` (12 tests at the time incl. per-branch CAS isolation, switch-never-loses-work, undo of a switch, 5 concurrent creates with one winner, 4 concurrent publishes to different branches), `bridge.rs::export_of_a_feature_branch_shares_history_with_main`, `cli_smoke.rs::branch_commands` |
| CI green on 3 OSes | run above |

What the R2.5 gate does **not** prove: merges across machines (R3), and that
a clean textual merge builds or passes tests (R4). Not implemented: branch
delete/rename, protected branches (R4), tags (R6).

## R2.5 branch-model refinements (owner review, 2026-11-01)

The owner approved the branch model in principle and asked for eight
refinements before full acceptance. Status of each:

| # | Refinement | Implementation | Evidence |
|---|---|---|---|
| 1 | Complete Git-compatible names, sequences, components, reserved patterns, namespace collisions | `refname.rs`: Git rule layer, portability policy, component-wise case/prefix collisions; checked again under the repository lock | `refname.rs` unit tests; `tests/refname_vs_git.rs` (≈460 names, identical verdicts to `git check-ref-format`; every accepted name is a valid Git branch). The test found that `@` alone is valid in Git's ref grammar but expanded to HEAD as a branch name, so it moved to the branch policy. A namespace bug (`Topic/y` vs `topic/x` both accepted) was found by `concurrent_creates_of_colliding_names_have_one_winner` and fixed |
| 2 | Branch creation preserves unpublished saves | default start = workspace head; `branch create --switch` (one operation, no file changes); invariant B1 | `create_switch_keeps_unpublished_saves_on_the_new_branch`, `switch_never_loses_work`, reachability assertions |
| 3 | `update` vs `merge <branch>` | `merge_branch`, CLI `concurv merge`, operation kind `merge`, undoable; `update` unchanged; unreleased `update --from` removed | `merge_another_branch_then_publish_to_own`, CLI `branch_commands` |
| 4 | Transactional switch: dirty trees, untracked and ignored collisions, incomplete merges, filesystem errors | transactional checkout (untracked plan, rollback, origin/branch marker, resume) | `switch_keeps_untracked_files_and_refuses_collisions`, `switch_never_overwrites_an_ignored_file`, `switch_is_refused_during_a_pending_merge`, `tests/branch_switch_faults.rs` (all 6 fault points × every hit × {transient, crash}: 43 transient failures, all rolled back to the exact original state or already committed; every crash state old-or-new and resumed), `interrupted_switch_can_be_finished_or_undone` |
| 5 | R3 reconciliation preserves divergence; no automatic publication | RFC 0003 §5 rewritten; `reconcile::plan` (pure, not wired to a command) | `tests/reconcile.rs` with an in-memory CAS relay |
| 6 | Tests: concurrent creation, several workspaces per branch, switching, unpublished changes, CAS failures, crash recovery, cross-machine divergence | — | `tests/branches.rs` (19), `branch_switch_faults.rs` (2), `reconcile.rs` (4), `refname_vs_git.rs` (2), CLI `kill_recovery.rs::killed_branch_switches_are_finished_by_running_them_again` (real processes; locally 20 kills, 18 mid-switch, all resumed) |
| 7 | Document workspace state, local branch refs, remote-tracking refs, authoritative heads | ADR 0007 §8, RFC 0004 §12, RFC 0003 §5, user guide §4a, architecture | docs |
| 8 | Delete/rename, protected branches, tags stay on the roadmap | not implemented | `ROADMAP_REVISED.md` P2-1, P2-8, P2-9, P2-10 |

Local run (Windows 11, rustc 1.95): 191 tests passed, 0 failed, 0 ignored;
clippy clean for Windows and the Linux target. CI evidence: pending (added
when the branch is merged).
