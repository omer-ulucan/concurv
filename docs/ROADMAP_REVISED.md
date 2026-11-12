# Concurv roadmap (revised 2026-11-01, canonical)

This is the **single source of truth for the plan**. `docs/ROADMAP_STATUS.md`
records only gate states and evidence. The original R0–R7 stages from the
project's initial research are preserved and refined here. The audit behind
this revision (an evidence-based capability matrix at `53a33ae`) is summarized
in §1 and in the status column of `docs/ROADMAP_STATUS.md`.

## 1. Why the plan changed

R1 and R2 passed their gates, but the audit found that Concurv is not yet a
usable *daily* VCS. It versions every file (no ignore support, so `target/`
and `node_modules/` are saved). `status` hashes every byte on each run, with
no measurement of the cost. There is one hard-coded branch, and two edits to
different functions in one file always conflict. Distributed sync (R3) would
multiply these problems across machines, so two stages are inserted:

- **R1.5 (Daily VCS usability and safety)**, before R3: ignores, safe
  incremental status, working-tree and diff correctness, filesystem hardening.
- **R2.5 (Local merge and branch semantics)**, in parallel with R3 design and
  before R3 implementation: hunk-level merge, which R3 reconciliation needs,
  and a branch model ADR. The stronger *verified* publication guarantee stays
  in R4.

## 2. Stages and gates

| Stage | Goal | Depends on | Gate (must be proven by tests/CI) |
|---|---|---|---|
| R0 | Specs + competitor reproduction | — | RFCs/ADRs written (done); reproducible scenario scripts for Git and at least one competitor; results recorded with versions |
| R1 | Local kernel | R0 specs | **passed** |
| **R1.5** | Daily VCS usability and safety | R1 | All R1.5 P0 items done: Git-compatible ignores incl. nested and negation, tracked-stays-tracked, ignored files never overwritten/deleted by restore/update; `status` baseline measured, then a stat cache that passes the false-clean suite (same size+mtime, mid-scan change, cache corruption); diff "no newline" and CRLF correct; macOS case-collision test; CI green on 3 OSes |
| R2 | Workspaces + concurrency | R1 | **passed** |
| **R2.5** | Local merge and branch semantics | R1.5, R2 | Hunk-level 3-way merge (diff3 style) merges disjoint same-file edits and conflicts on overlap, reusing the conflict object model and existing commands; branch-model ADR accepted; smallest branch slice (create/list/switch-by-workspace) implemented or explicitly blocked |
| R3 | Two-machine authenticated sync | R2.5 merge, RFC 0003 revision | Two independent repos save offline, reconnect in both orders; all acked snapshots on the relay; one authoritative head; stale publish → explicit conflict; duplicate/reordered messages idempotent; disconnect, malformed object, auth failure, permission denial, crash/restart tests; local two-process harness first |
| R4 | Verified integration | R2.5, R3 | Candidate-hash + policy + environment-bound attestation; stale attestation rejected; failing merged test blocks publish; no TOCTOU; sandbox policy explicit |
| R5 | Optional agent coordination | R3 (events), R4 | Versioned JSONL events, advisory intent with TTL, overlap warnings with measured false-positive/negative rates; no LLM dependency |
| R6 | Public beta + Git ecosystem | R1.5, R2.5, R3 | Release pipeline (3 OSes), Git tags, import into existing history, remote workflow, completion/config, CONTRIBUTING/SECURITY, **owner-approved LICENSE** |
| R7 | Advanced, demand-driven | benchmarks | GC preserving reachability/backups/conflicts/unpublished/incoming; packs; rename detection; sparse/partial; large files. Each item must beat a measured baseline |

## 3. Backlog

Each item lists: **milestone** · acceptance test · failure/rollback behaviour ·
completion evidence.

### P0 (blocks R1.5)

| Id | Item | Milestone | Acceptance test | Failure / rollback | Evidence when done |
|---|---|---|---|---|---|
| P0-1 | `.gitignore` matching (root + nested, negation, anchors, `/` dirs, `**`, escapes) in the native scanner | R1.5 | `ignore.rs`: `target/`, `node_modules/`, `.env` excluded; nested override; `!` re-include; excluded parent cannot be re-included (Git semantics) | ignore parsing errors surface as clear errors, never silently include or exclude | tests + CI run id |
| P0-2 | Tracked-stays-tracked (tracked = in workspace head) | R1.5 | tracked file later matched by an ignore rule is still saved; its deletion still shows as deleted | no format change; disabling the engine reverts to "everything versioned" | tests |
| P0-3 | Local exclude file `.concurv/exclude` (per workspace, not versioned) | R1.5 | patterns apply after all `.gitignore` levels | missing file = no rules | tests |
| P0-4 | Ignored/untracked files never overwritten or deleted by restore/update/undo/workspace checkout | R1.5 | target writes a path occupied by an ignored file → refused, file intact; ignored files inside deleted directories keep the directory | refusal (exit 20), nothing modified | tests |
| P0-5 | `concurv check-ignore <path>… [--json]` | R1.5 | reports ignored/tracked, deciding file and pattern | read-only | CLI test |
| P0-6 | `status` benchmark harness (100/1k/10k files, mixed binary and ignored trees), p50/p95 | R1.5 | `bench/` script reproducible from a seed; results with hardware/toolchain/FS | n/a | `bench/RESULTS.md` |
| P0-7 | Safe stat cache (disposable, never authoritative; racy-timestamp guard; full verify option) | R1.5 | false-clean suite: same size+mtime rewrite, change during scan, cache corruption/eviction, symlink retarget, 2 workspaces, Windows; before/after numbers on the P0-6 fixtures | corrupt or unreadable cache → ignored and rebuilt; `status --no-cache` forces full hashing | tests + measured numbers |
| P0-8 | Diff: "\ No newline at end of file", CRLF preserved, binary summary | R1.5 | golden diff tests | n/a | tests |
| P0-9 | macOS case-collision refusal test | R1.5 | CI on macOS | n/a | CI |

### P1

| Id | Item | Milestone | Acceptance test | Failure / rollback | Evidence |
|---|---|---|---|---|---|
| P1-1 | Hunk-level 3-way merge reusing the conflict model; markers per hunk | R2.5 | disjoint hunks merge; adjacent/overlapping conflict; identical edits; EOF insert; no final newline; CRLF; binary → whole-file conflict; crash during materialization; Git round trip | any doubt → conflict, never a silent pick | tests |
| P1-2 | Branch model ADR (named branches vs workspaces, switch semantics, publish targets, sync implications) | R2.5 | ADR accepted by the owner (done: ADR 0007, accepted under the owner's delegation) | — | ADR |
| P1-3 | Minimal branches: `branch create/list/switch`, workspace on a branch, publish to it, `merge <branch>`, transactional switch, Git-compatible names (ADR 0007 refinements) | R2.5 | tests for isolation and CAS per branch | refuse ambiguous operations | tests |
| P1-4 | RFC 0003 revision (wire schema, device identity, ACLs, TLS, negotiation, quarantine, resume, replay defense) | R3 prep | RFC review | — | RFC (done 2026-11-12: revision 2, implemented as protocol version 1) |
| P1-5 | Fuzz targets for decoders and the fast-export parser | R1.5–R2.5 | `cargo fuzz` runs recorded (not just configured) | — | corpus + run log |
| P1-6 | Explicit `track` / `untrack` with persisted state | R2.5 | ADR + tests: untrack keeps the file on disk | — | tests |
| P1-7 | Side-by-side Git runs for S11/S13 | R0 | scripts under `bench/scenarios/` with transcripts | — | results file |
| P1-8 | `workspace remove` (safe, refuses with unpublished work) | R2.5 | tests | refuse | tests |

### P2

| Id | Item | Milestone |
|---|---|---|
| P2-1 | Git tags; import into existing history; native remote workflow | R6 (tags and import into existing history done 2026-11-12: ADR 0008, ADR 0002 update; the remote workflow is R3's relay. Not yet: tags at the relay) |
| P2-2 | Release pipeline, shell completion, config file, CONTRIBUTING/SECURITY/CoC/templates | R6 (completions, configuration files and contributor documents done 2026-11-12; release pipeline local for Windows and Linux, macOS needs hosted CI or a Mac) |
| P2-3 | GC and packs (with grace periods and reachability proofs) | R7 |
| P2-4 | Rename detection, sparse/partial checkout, large files | R7 |
| P2-5 | Filesystem watcher for `status` | R7 |
| P2-6 | Agent identity, intent, JSONL events | R5 (done 2026-11-12: RFC 0006; identity = author and workspace name) |
| P2-7 | Candidate test gates, attestations, sandbox | R4 (done 2026-11-12: RFC 0005; sandbox "none" stated explicitly, isolation open) |
| P2-8 | Branch delete and rename (policy for workspaces tracking them; sync semantics; refuse while tracked or unmerged) | R3 (done 2026-11-12: ADR 0007 §9; local only, a relay keeps its branches) |
| P2-9 | Protected branches (publication only through R4 gates) | R4 (done 2026-11-12: `protect` in the branch head's policy, locally and at the relay) |
| P2-10 | Remote-tracking references in the view (`remote/<relay>/<b>`, separate from `branches`) and wiring `reconcile::plan` into fetch | R3 (done 2026-11-12: `View::remotes`, repository format 2) |

## 4. Execution order

1. Gate 1 (this document plus audits) ✔.
2. R1.5: P0-1…P0-5 (ignore engine and restore safety), then P0-6 (measure),
   then P0-7 (stat cache), then P0-8/P0-9.
3. R2.5: P1-1 hunk merge, then P1-2 ADR, then P1-3.
4. R3: revise RFC 0003, then build a local two-process harness, then implement ✔ (2026-11-12).
5. R4 ✔ (2026-11-12) → R5 ✔ (2026-11-12) → R6 → R7 as gated above.

Open owner decisions: **LICENSE** (none is asserted: the license metadata was
removed pending the owner's choice), the branch model (P1-2), and whether
`.concurvignore` is ever needed
(not planned: `.gitignore` plus `.concurv/exclude` cover the documented cases).
