# concurv roadmap status

Legend: `not started` · `in progress` · `gate passed` · `blocked`.
A stage is `gate passed` only with the evidence listed next to it.
The plan itself (stages, gates, backlog, acceptance tests) lives in
**`docs/ROADMAP_REVISED.md`**. Last updated: 2026-11-12 (R4 verified integration).

| Stage | Status | Evidence / blockers |
|---|---|---|
| R0 — RFC & competitive reproduction | **in progress** | Done: RFC 0001–0003, ADR 0001–0003, threat model, comparison plan with exact expected behaviour for S1–S10. **Open gate items:** competitor harness (`bench/scenarios/`) not built; **no** Git/jj/but/gitta/Fossil/Pijul scenario reproduced. |
| R1 — Native local VCS kernel | **gate passed** (2026-11-01) | CI run [41350688459](https://github.com/omer-ulucan/concurv/actions/runs/41350688459) on `main` @ `9b8ed23`: all gate checks pass on Linux, macOS and Windows. See evidence below. |
| R1.5 — Daily VCS usability & safety | **gate passed** (2026-11-01) | CI run [41366633305](https://github.com/omer-ulucan/concurv/actions/runs/41366633305) on `main` @ `19ce0ed`: ignores (ADR 0004), ignored-file safety, `check-ignore`, benchmark, stat cache (ADR 0005), diff and case-collision tests, green on Linux, macOS and Windows. See "R1.5 evidence" below |
| R2 — Private workspaces + concurrency | **gate passed** (2026-11-01) | CI run [41359891403](https://github.com/omer-ulucan/concurv/actions/runs/41359891403) on `main` @ `0689e96`: all four slices (RFC 0004, ADR 0002) green on Linux, macOS and Windows; 2/5/10 concurrent writers and killed-process tests pass. Git tags not supported (scoped). See "R2 evidence" below. |
| R2.5 — Local merge & branch semantics | **in progress** (gate re-opened 2026-11-01) | Line-level merge (ADR 0006) verified. Branch model: the owner approved ADR 0007 **in principle** and requested refinements; they are implemented and tested (see "R2.5 branch-model refinements" below). Remaining gate item: the owner's final acceptance of ADR 0007 |
| R3 — Two-PC authenticated sync | **gate passed** (2026-11-12, Windows and Linux) | Local CI gate on `3f85c0b`: Windows 216 passed, Linux 220 passed; relay `concurvd`, pinned TLS, devices and scopes, fetch/push/clone, two-process harness, disconnect, malformed-object, auth, crash/restart tests (RFC 0003 v1). macOS not run. See "R3 evidence" below |
| R4 — Verified integration | **gate passed** (2026-11-12, Windows and Linux) | Local CI gate on `5a4863b`: Windows 227 passed, Linux 231 passed; protected branches, `concurv check`, attestations bound to candidate, branch, policy version, checks and OS, at the relay too (RFC 0005). macOS not run. See "R4 evidence" below |
| R5 — Agent-aware coordination | not started | — |
| R6 — Developer beta + Git ecosystem | not started | design only: ADR 0002 |
| R7 — Advanced VCS | not started | — |

## How evidence is produced (since 2026-11-12)

GitHub Actions is not used for now (its workflows start only manually).
Gate evidence comes from the **local CI gate**, `scripts/ci-all.sh`: the
steps of `.github/workflows/ci.yml` (fmt, clippy `-D warnings`, every test
with `--no-fail-fast`, release build, MSRV 1.89 `cargo check`, all
`--locked`) on a fresh clone of the commit, in parallel on Windows 11
(NTFS) and in WSL 2 (Ubuntu 26.04, ext4), with the same rustc on both.
Each run records the commit, host and toolchain in its summary. **macOS is
not covered** by the local gate; the last macOS evidence is the hosted run
of 2026-11-02 (`79caf3f`). Evidence older than this section comes from
hosted CI runs, linked where cited.

First local runs: `main` @ `96e1102` and the local-CI commits
`3b8d516` and `0bf5bb6`, all PASS with rustc 1.99.1 on both sides (Windows
191 tests, WSL 195; 0 failed, 0 ignored). The first WSL run found that
`killed_processes_never_lose_acknowledged_saves` killed no process on a
fast machine (its fixed kill window was longer than the commands), so the
test failed its own check without testing anything; its kill moments are
now calibrated to measured command durations.

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

R1, R1.5, R2, R3 and R4 are complete; R2.5 awaits the owner's final acceptance
of ADR 0007; R0 (competitor harness) remains open. Next: R5, agent
coordination (`docs/rfcs/0006-agent-coordination.md`), then R6.
macOS evidence needs hosted CI or a Mac running `scripts/ci-local.sh`.

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

CI run [41375647699](https://github.com/omer-ulucan/concurv/actions/runs/41375647699)
on `main` @ `79caf3f`, rustc 1.99.1, `--locked`, `--no-fail-fast`: ubuntu-latest
**195 passed**, macos-latest **195 passed**, windows-latest **191 passed**
(Unix-only tests differ), 0 failed, 0 ignored; MSRV 1.89 `cargo check` pass.
The crash tests assert that kills actually landed mid-switch and were
resumed, and that transient failures were actually rolled back, so a green
run proves both happened on every OS.

**Verdict:** every refinement is implemented and its invariants (ADR 0007
§6, B1–B5) are verified by the tests above on all three platforms. ADR 0007
is *approved in principle*; marking it fully accepted, and with it the
R2.5 gate, is the owner's decision.

## R3 evidence (gate passed 2026-11-12, Windows and Linux)

Local CI gate (`scripts/ci-local.sh`, on Windows and then in WSL, the same
commit) on `feat/r3-sync` @ `3f85c0b`, rustc 1.99.1
on both sides, `--locked`, `--no-fail-fast`:

| Platform | Tests |
|---|---|
| Windows 11, NTFS | **216 passed, 0 failed, 0 ignored** |
| Linux (WSL 2, Ubuntu 26.04, ext4) | **220 passed, 0 failed, 0 ignored** (4 Unix-only tests more) |
| MSRV 1.89 `cargo check` | pass on both |
| macOS | **not run** (no hosted CI; see "How evidence is produced") |

Gate statement (roadmap §2): *"Two independent repos save offline,
reconnect in both orders; all acked snapshots on the relay; one
authoritative head; stale publish → explicit conflict; duplicate/reordered
messages idempotent; disconnect, malformed object, auth failure, permission
denial, crash/restart tests; local two-process harness first."*

| Requirement | Tests (`crates/concurv-sync/tests/` unless noted) |
|---|---|
| Two independent repositories save offline and reconnect, in both orders | `two_machines.rs::two_machines_save_offline_and_reconnect_in_both_orders` (each order: the second push is refused, fetch preserves the divergence, `merge remote/origin/main`, publish, push), `fetch_then_update_integrates_unpublished_saves`; CLI `sync_cli.rs::two_repositories_sync_through_a_relay_process` |
| All acknowledged snapshots on the relay | history assertions in the tests above; `racing_pushes_have_one_winner_and_all_work_lands_eventually` (5 clients); CLI `killed_relay_and_killed_clients_never_lose_acknowledged_work`: every acknowledged save and push is in the relay's `main` after 4 relay kills and about 11 client kills per run |
| One authoritative head | the relay's `state.json` compare-and-swap; racing pushes have exactly one winner per round |
| Stale publish is an explicit conflict | `stale_publish_is_refused_and_retries_are_idempotent` (`CONCURV_STALE_HEAD`, exit 41, naming the observed head); push refused with exit 42 when the relay has unfetched or diverged work; forward-only and incomplete-graph refusals |
| Duplicate and reordered messages are idempotent | same test: every object uploaded twice in reverse dependency order; a publish retried with its request id is answered from the relay's memory and applied once; reusing a request id for another publish is refused |
| Disconnect | `disconnects_during_push_lose_nothing_and_never_publish_twice` (cuts across the whole push and at each of its last 40 bytes; about 20 cuts lose the publish reply, every retry is answered as `replayed` and the relay's `seq` shows one publish), `disconnects_during_fetch_change_nothing` (25 cuts: nothing adopted, no reference changed, the retry completes) |
| Malformed object | `hostile.rs::malformed_traffic_is_refused_and_changes_nothing` (bad frames, JSON, versions, corrupt and wrong-kind objects, truncation, limits), `a_lying_relay_cannot_inject_objects_or_references` (forged, incomplete, wrong-kind and oversized objects; invalid and colliding names); `concurv-objects/tests/transfer.rs` |
| Authentication failure, permission denial | `authentication_and_permissions_are_enforced` (unknown, malformed, revoked tokens; read-only device; `publish:main` only), `a_wrong_certificate_stops_the_client_before_the_token_is_sent` (no application byte reaches the impostor); CLI exit codes 73, 74, 75 |
| Crash and restart | `sync_faults.rs`: an injected failure at every write step of a relay publish (160 cases) and of a client fetch (about 290 cases), as crash and as transient error: old or new state only, retry completes once; CLI kill test with real `concurvd` processes killed mid-push and restarted |
| Local two-process harness first | `concurv-cli/tests/sync_cli.rs`: real `concurvd` and `concurv` processes over TLS on loopback |
| Also | `a_rewound_relay_is_reported_and_never_followed`; `format_upgrade_keeps_secondary_workspaces_working` (format 1 → 2, secondary workspaces, undo after fetch) |

**Across two operating systems** (manual run, 2026-11-12): relay in WSL
(Linux, ext4); one client on Windows (NTFS) connecting through WSL's
localhost forwarding, one in WSL. Clone, push, clone on the other side
(CRLF bytes identical), concurrent offline work: the Windows push was
refused with exit 42, `fetch` reported `diverged`, `merge
remote/origin/main` + `publish` + `push` succeeded, the Linux side
fast-forwarded; the executable bit set on Linux survived a merge made on
Windows; `verify --full` passed everywhere.

Found and fixed while building R3: a malicious relay can advertise names
that collide with each other; fetch now creates only the first (in byte
order) and reports the rest. An upload refused for permissions used to
close the connection while the client was still sending, so the client saw
a network error; the relay now answers `ready` before objects are sent.
The fetch fault test assumed a fixed number of write steps (random ids
change it by one or two), which the first WSL run caught.

What the R3 gate does **not** prove: macOS (not run since R2.5); real
wide-area networks (latency, NAT, middleboxes); a relay under sustained load
or long uptime; relay high availability; power loss (fault injection
simulates crashes of the process, not of the machine).

## R4 evidence (gate passed 2026-11-12, Windows and Linux)

Local CI gate (`scripts/ci-local.sh`, Windows and then WSL, same commit) on
`feat/r4-verified` @ `5a4863b`, rustc 1.99.1, `--locked`, `--no-fail-fast`:
Windows 11 **227 passed**, Linux (WSL 2) **231 passed**, 0 failed, 0
ignored; fmt, clippy `-D warnings`, release build and MSRV 1.89 check pass
on both. macOS not run.

Gate statement (roadmap §2): *"Candidate-hash + policy +
environment-bound attestation; stale attestation rejected; failing merged
test blocks publish; no TOCTOU; sandbox policy explicit."*

| Requirement | Tests |
|---|---|
| Candidate-hash + policy + environment-bound attestation | object kind 6 binds candidate, branch, policy blob, checks and environment (RFC 0005 §4); `verified_integration.rs::attestations_record_their_environment` (an attestation from another OS does not qualify) |
| Stale attestation rejected | `stale_attestations_are_refused`: another candidate, a newer policy version, another branch; a forged result contradicting the exit codes cannot even be decoded |
| Failing merged test blocks publish | `a_failing_merged_test_blocks_publication`: two workspaces each pass alone, the merge fails, nothing is published, the green attestation of an unmerged head never authorizes the merge; at the relay, `protected.rs` (missing, unvouched, failing and other-candidate attestations refused; devices without `attest` cannot vouch) |
| No TOCTOU | `checks_see_the_candidate_not_the_working_copy`: checks run on a fresh checkout of the snapshot (an ignored file on disk does not help), and `publish` moves the branch to exactly the attested snapshot under the repository lock |
| Sandbox policy explicit | the policy must state `"sandbox": "none"` (unknown values refused); `untrusted_policies_never_run`: nothing runs and nothing is written until this policy version is approved on the device |
| Also | a candidate cannot weaken its own policy (`a_candidate_cannot_weaken_its_own_policy`); timeouts, missing programs, invalid policies and foreign-OS policies fail closed; CLI exit codes 44/45 and JSON (`verified_cli.rs`) |

What the R4 gate does **not** prove: that a runner really ran the checks
(attestations are claims of an authenticated device; signatures are R7);
isolation of checks (`sandbox: none`); macOS.
