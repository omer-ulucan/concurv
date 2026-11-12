# Competitive reproduction and comparison plan

Status: **Harness built; concurv and Git reproduced for S04, S05, S09, S10
and S12 on Windows and Linux (2026-11-12, `bench/results/`). No other
competitor has been run yet** (jj scripts exist but have not run; the R0
gate needs at least one). Apart from the "Reproduced" section, no result
in this file is a measurement. Rows labelled *literature* come from
the project docs/issues listed under "Literature sources" at the end of this file;
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
| S1 | Concurrent edits, same file, two agents | repo with `src/lib.rs`; agents A and B each edit a different function in private workspaces | both saves acknowledged in their own workspaces; neither working copy is modified by the other; integration produces a clean 3-way merge | **implemented**: different files (R2 slice 2a, `workspaces.rs`) and disjoint edits in one file (R2.5, ADR 0006, `hunk_merge.rs`) |
| S2 | Shared API change | A renames `fn parse()` → `fn parse_v2()`; B adds a new caller of `parse()` | textual merge clean; combined test fails; publish refused with `CONCURV_POLICY_FAIL`; both changes retained | R4 (+R5 warning) |
| S3 | Tests green individually, red merged | as S2 with `cargo test` gate | attestation for the *candidate hash* is FAIL; neither branch's green result authorizes the candidate | R4 |
| S4 | Concurrent undo | A and B each save twice; A undoes | only A's workspace operation is reverted; B's acknowledged snapshots unaffected | **R2 slice 2b — implemented** (`concurv-workspace/tests/undo.rs::undo_refuses_publish_and_never_touches_other_workspaces`, concurrent save+undo in `concurrency.rs`) |
| S5 | Simultaneous branch publication | A and B publish to `main` with the same expected head | exactly one succeeds; the other gets `CONCURV_STALE_HEAD` with observed head; no lost snapshot | **implemented**: local CAS (R2, `concurrency.rs`) and relay CAS with `CONCURV_STALE_HEAD` naming the observed head (R3, `concurv-sync/tests/two_machines.rs`) |
| S6 | Offline reconnect | A and B save offline, reconnect in both orders | all snapshots present on relay; one published head; divergence visible, not overwritten | **R3 — implemented** (`two_machines.rs`, CLI `sync_cli.rs`) |
| S7 | Long transcript overhead | agent session of 10k events | concurv stores no transcript by default; metadata growth bounded | R5 |
| S8 | Interrupted save (crash during snapshot) | kill/fail at each write boundary | previous acknowledged head intact; no ref to missing object; re-save succeeds | **R1 — implemented as deterministic fault-injection tests** (`crates/concurv-workspace/tests/fault_injection.rs`) |
| S9 | Restore over dirty working copy | modify file, restore older snapshot | refused without `--force`; with `--force` a backup snapshot is recorded | **R1 — implemented** (`crates/concurv-cli/tests/cli_smoke.rs`, `crates/concurv-workspace/tests/restore_safety.rs`) |
| S10 | Snapshot misses data with unchanged mtime/size (cf. jj #10509) | rewrite a file with same size within mtime granularity | change detected (concurv hashes content; no stat cache in R1) | **R1 — implemented** (`crates/concurv-workspace/tests/engine.rs::status_detects_same_size_same_mtime_change`) |
| S11 | Nested ignores; tracked-then-ignored | root + nested `.gitignore` with `!`, excluded parent, tracked file later matched | Git semantics: deeper file wins, excluded parent not re-includable, tracked files stay tracked | **R1.5 — implemented** (`crates/concurv-workspace/tests/ignore.rs`); **reproduced against Git** for the matching rules (`ignore_vs_git.rs`, 7 cases) |
| S12 | Same file, disjoint hunks | A edits the top function, B the bottom one, same file | merges cleanly; overlapping edits conflict | **R2.5 — implemented** (ADR 0006, `hunk_merge.rs`) |
| S13 | `status` on 1k/10k files; same-size same-mtime edit | generated trees | correct (no false clean); latency measured | R1.5 (`bench/`) |
| S14 | Ignored/untracked file survives restore/update | ignored file where the target writes | refused, file intact | **R1.5 — implemented** (`ignore.rs::restore_never_overwrites_an_ignored_file_even_with_force`, `update_refuses_to_overwrite_an_ignored_file_in_another_workspace`) |
| S15 | Git round trip limits | merge, binary, Unicode, divergence | trees and metadata preserved, documented losses | **R2 — implemented** (`crates/concurv-git/tests/bridge.rs`) |
| S16 | Two-PC offline divergence and reconnect | two repos, relay | one authoritative head; nothing lost | **R3 — implemented** (`two_machines.rs`, `sync_cli.rs`, `sync_faults.rs`) |
| S17 | Candidate-hash-bound failing merged test | two green branches, red merge | publish refused | R4 |

## Competitor capability labels (literature, not reproduced)

| Scenario | Git | jj | but | gitta | Fossil | Pijul |
|---|---|---|---|---|---|---|
| S1 isolation | Y (worktree) | Y (workspaces) | P | Y | Y (multiple checkouts) | Y |
| S4 scoped undo | P (reflog per worktree HEAD) | Y, issues #10476/#10480 reported | Y | E | P | P |
| S5 CAS publish | Y (`update-ref` old-value, push lease) | E (git remote) | E | E | P (autosync) | ? |
| S10 content-based detection | Y (racy-git handling) | reported bug #10509 | ? | E | ? | ? |

Y = documented, P = partial, E = via external tool, ? = unverified.

## Harness

`bench/scenarios/S<id>-<name>/<tool>.sh` (POSIX shell, fresh temporary
directory, hermetic configuration) prints `RESULT: <pass|partial|fail|unsupported>
<detail>`; `bench/scenarios/run.sh` writes `bench/results/<date>-<os>.md` with
the environment record. See `bench/scenarios/README.md`.

## Reproduced (2026-11-12)

concurv `23cb8e5` (0.1.0-alpha.3 code); Git 2.46.0.windows.1 on Windows 11
(NTFS) and Git 2.53.0 on Ubuntu 26.04 in WSL 2 (ext4). Full rows:
`bench/results/2026-11-12-mingw64.md`, `bench/results/2026-11-12-linux.md`.

| Scenario | concurv (both OSes) | Git, Windows | Git, Linux |
|---|---|---|---|
| S04 scoped undo | pass | partial: no undo; a reflog reset in A's worktree reverts only A | partial (same) |
| S05 CAS publish | pass (second publish refused, exit 42) | pass (`update-ref` with old value) | pass |
| S09 restore over unsaved edit | pass (refused; `--force` keeps a backup) | fail: `git restore --source` overwrote the edit, no backup | fail (same) |
| S10 same size, same mtime | pass | **fail: reported clean** | pass |
| S12 disjoint hunks | pass (overlap refused, exit 43) | pass (overlap: conflict markers) | pass |

Notes. S09 uses `git restore --source=<commit> -- <file>`, the command that
restores files; `git checkout <commit>` of a whole tree is a different
operation that refuses to overwrite local changes. S10 on Windows: after
the rewrite, size and modification time match the index and Git for
Windows reported the file unchanged; on Linux the change time (which a
rewrite always updates) exposed it. This is one machine and one Git
version per OS, not a general claim about Git.

## Literature sources (retrieved 2026-11-01)

The *documented* labels above refer to these sources. They are literature, not
reproductions.

### Official docs and repos
- Git worktrees: https://git-scm.com/docs/git-worktree
- Git CAS reference updates: https://git-scm.com/docs/git-update-ref
- Git partial clones: https://git-scm.com/docs/partial-clone
- Jujutsu README: https://github.com/jj-vcs/jj/blob/main/README.md
- Jujutsu operation log: https://github.com/jj-vcs/jj/blob/main/docs/operation-log.md
- Jujutsu working-copy model: https://github.com/jj-vcs/jj/blob/main/docs/working-copy.md
- Jujutsu Git interoperability caveats: https://github.com/jj-vcs/jj/blob/main/docs/git-compatibility.md
- GitButler CLI: https://docs.gitbutler.com/cli-overview
- GitButler repository/license: https://github.com/gitbutlerapp/gitbutler
- gitta docs: https://gitta.io/docs/
- gitta internals: https://gitta.io/docs/how-it-works
- Entire CLI: https://github.com/entireio/cli
- Entire security: https://github.com/entireio/cli/blob/main/docs/security-and-privacy.md
- Pijul change theory: https://pijul.org/manual/why_pijul.html
- Pijul conflicts: https://pijul.org/manual/conflicts.html
- Fossil design: https://fossil-scm.org/home/doc/tip/www/index.wiki
- Fossil sync protocol: https://www.fossil-scm.org/home/doc/trunk/www/sync.wiki
- Container Use: https://github.com/dagger/container-use
- GitHub stacked PRs: https://docs.github.com/en/pull-requests/reference/stacked-pull-requests

### Selected incident reports and user discussions
- Jujutsu snapshot issue #10509: https://github.com/jj-vcs/jj/issues/10509
- Jujutsu concurrent op #10480: https://github.com/jj-vcs/jj/issues/10480
- Jujutsu workspace undo #10476: https://github.com/jj-vcs/jj/issues/10476
- Jujutsu colocated index #9711: https://github.com/jj-vcs/jj/issues/9711
- GitButler resolution #15613: https://github.com/gitbutlerapp/gitbutler/issues/15613
- Entire storage blowup #2663: https://github.com/entireio/cli/issues/2663
- Entire worktree cross-session #2979: https://github.com/entireio/cli/issues/2979
- Entire public transcript/redaction #2951: https://github.com/entireio/cli/issues/2951
- Reddit multiple agents/worktrees: https://www.reddit.com/r/ClaudeAI/comments/1qzduim/stop_running_multiple_claude_code_agents_in_the/
- Reddit agent status awareness: https://www.reddit.com/r/ClaudeCode/comments/1vtfawm/how_are_you_running_multiple_coding_agents_at_the/
- Reddit merged code tests fail: https://www.reddit.com/r/AI_Agents/comments/1wntjk6/a_clean_git_merge_of_my_two_agents_worktrees_that/
- Reddit isolation limits: https://www.reddit.com/r/AI_Agents/comments/1v53mns/running_multiple_coding_agents_in_parallel_broke/

### Benchmark (tool vendor maintains it; not independent)
- https://vcbench.dev/
- https://github.com/gitbutlerapp/version-control-bench
