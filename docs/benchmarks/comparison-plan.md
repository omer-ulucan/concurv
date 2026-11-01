# Competitive reproduction and comparison plan

Status: **Plan written; no competitor scenario has been reproduced yet.**
No result in this file is a measurement. Rows labelled *literature* come from
the project docs/issues cited in `docs/ARC_Research_and_Roadmap_2026.md` §14;
they become *reproduced* only after a script under `bench/scenarios/` runs and
its output is committed together with tool versions and OS.

Tools: Git (2.46.0.windows.1 is available on the dev machine), Jujutsu `jj`,
GitButler `but`, gitta, Fossil, Pijul. Versions must be pinned at run time.

## Environment record (required for every run)

OS + version, filesystem, CPU, RAM, tool versions (`git --version`,
`jj --version`, …), concurv commit hash, exact command lines.

## Scenarios and exact expected concurv behaviour

| ID | Scenario | Fixture | Expected concurv behaviour (target) | concurv stage |
|---|---|---|---|---|
| S1 | Concurrent edits, same file, two agents | repo with `src/lib.rs`; agents A and B each edit a different function in private workspaces | both saves acknowledged in their own workspaces; neither working copy is modified by the other; integration produces a clean 3-way merge | R2/R4 |
| S2 | Shared API change | A renames `fn parse()` → `fn parse_v2()`; B adds a new caller of `parse()` | textual merge clean; combined test fails; publish refused with `CONCURV_POLICY_FAIL`; both changes retained | R4 (+R5 warning) |
| S3 | Tests green individually, red merged | as S2 with `cargo test` gate | attestation for the *candidate hash* is FAIL; neither branch's green result authorizes the candidate | R4 |
| S4 | Concurrent undo | A and B each save twice; A undoes | only A's workspace operation is reverted; B's acknowledged snapshots unaffected | R2 |
| S5 | Simultaneous branch publication | A and B publish to `main` with the same expected head | exactly one succeeds; the other gets `CONCURV_STALE_HEAD` with observed head; no lost snapshot | R2 (local CAS) / R3 (relay) |
| S6 | Offline reconnect | A and B save offline, reconnect in both orders | all snapshots present on relay; one published head; divergence visible, not overwritten | R3 |
| S7 | Long transcript overhead | agent session of 10k events | concurv stores no transcript by default; metadata growth bounded | R5 |
| S8 | Interrupted save (crash during snapshot) | kill/fail at each write boundary | previous acknowledged head intact; no ref to missing object; re-save succeeds | **R1 — implemented as deterministic fault-injection tests** (`crates/concurv-workspace/tests/fault_injection.rs`) |
| S9 | Restore over dirty working copy | modify file, restore older snapshot | refused without `--force`; with `--force` a backup snapshot is recorded | **R1 — implemented** (`crates/concurv-cli/tests/cli_smoke.rs`, `crates/concurv-workspace/tests/restore_safety.rs`) |
| S10 | Snapshot misses data with unchanged mtime/size (cf. jj #10509) | rewrite a file with same size within mtime granularity | change detected (concurv hashes content; no stat cache in R1) | **R1 — implemented** (`crates/concurv-workspace/tests/engine.rs::status_detects_same_size_same_mtime_change`) |

## Competitor capability labels (literature, not reproduced)

| Scenario | Git | jj | but | gitta | Fossil | Pijul |
|---|---|---|---|---|---|---|
| S1 isolation | Y (worktree) | Y (workspaces) | P | Y | Y (multiple checkouts) | Y |
| S4 scoped undo | P (reflog per worktree HEAD) | Y, issues #10476/#10480 reported | Y | E | P | P |
| S5 CAS publish | Y (`update-ref` old-value, push lease) | E (git remote) | E | E | P (autosync) | ? |
| S10 content-based detection | Y (racy-git handling) | reported bug #10509 | ? | E | ? | ? |

Y = documented, P = partial, E = via external tool, ? = unverified.

## Harness plan

`bench/scenarios/<id>/<tool>.sh` — POSIX shell, creates a fresh temp dir,
prints `RESULT: <pass|fail|unsupported> <detail>` and exits. A driver collects
results into `bench/results/<date>-<host>.md`. Not yet created; tracked in
`docs/ROADMAP_STATUS.md` as the remaining R0 gate item.
