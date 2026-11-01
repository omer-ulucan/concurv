# ARC — Agent-Native Distributed Version Control

**Research + Product Requirements + Architecture RFC + Rust Engineering Roadmap**  
**Date:** 2026-11-01  
**Status:** Proposal / RFC v0.1 — not implemented  
**Working name:** `arc` (availability, trademarks and CLI naming not checked)

## 0. Executive decision

Build a **CLI-first, local-first, agent-independent distributed VCS**, which is useful without AI but deliberately designed for several independent workspaces on different machines contributing to a **single logical shared branch**. Preserve immutable changes, separate branch publication from local work, and make safe integration an explicit verified operation.

Do **not** call this a new category merely because it supports agents: `git worktree`, Jujutsu, GitButler, gitta and Entire already cover significant portions of the problem. Compete with demonstrable integrity, usability and interoperability. Do not implement model-based semantic merge as the initial solution to distributed concurrency.

**North star:** `1 logical branch; N private workspaces; N devices; 0 acknowledged changes silently lost; deterministic integration; authenticated synchronization.`

**Important qualification:** “No data loss” is scoped to changes **durably acknowledged by ARC**. Unsaved editor buffers, untracked files explicitly excluded by policy, compromised endpoints, and catastrophic loss of every replica without backup are outside any honest absolute guarantee.

## 1. Research methodology and limitations

Inspected primary upstream project docs and READMEs, issue trackers, selected public developer reports and official product benchmark material as of 2026-11-01. Distinguish:

* **Documented capability:** project publishes a supported feature; not equivalent to independently benchmarked reliability.
* **Reported incident:** open issue or anecdote; not evidence of failure frequency or every version being affected.
* **Inference/product requirement:** design conclusion for ARC, not a claim that every competitor lacks it.

This is a targeted technical market survey, **not** a representative survey of developer preferences, a source-code audit of every competitor, or an exhaustive enumeration of bugs. Several products are changing weekly. Competitive claims need hands-on reproduction before marketing.

## 2. Competitive map

### 2.1 Version-control engines

| Project | Technical model | Documented advantage | Important qualification for ARC |
|---|---|---|---|
| Git | Distributed immutable object DAG; local branches, remote refs, worktrees | Ubiquitous hosting/ecosystem; mature SHA object store, CAS refs, partial clone | Agent intent and cross-workspace semantics are external; Git can isolate worktrees, so do not claim no concurrency |
| Jujutsu (`jj`) | Git-backed production-ready backend; working-copy-as-commit; operation DAG | Automatic snapshots, operation log, undo, conflicts-as-data, workspaces | Cross-machine live coordination not the central UX; mixed mutating git/jj may complicate operation history |
| GitButler (`but`) | Rust Git-based engine; CLI + desktop | Stacked/parallel branches, undo, commit reshaping, agent tools | Strong CLI UX incumbent; virtual branch ≠ complete cross-device agent integration guarantee |
| Sapling (`sl`) | Meta SCM, Git interop, large-repo-oriented operations | Smartlog and stacked work | Consider large repository design, not agent-native shared branch coordination |
| Pijul | Patch-theory/change graph | Independent changes commute when valid, first-class conflicts, persistent change identity | Harder model to implement rigorously; formal merge guarantees do not ensure program semantics |
| Mercurial | Mature DVCS | Changesets, bookmarks, evolution workflows | Useful history and ergonomics benchmark |
| Fossil | DVCS + embedded SQLite + autosync + bundled hosting | Transactional durability, integrity checks, easy sync, single binary | Autosync is already an idea; ARC must address agent-specific semantics, not rebrand autosync |

### 2.2 Adjacent agent technologies

| Product | Capability documented | ARC response |
|---|---|---|
| gitta | Dependency-aware notifications, structural merge, combined-test gate; cross-machine coordination in enterprise edition | Baseline to equal/exceed for actual concurrency use cases; not an unfilled niche |
| Entire | Checkpoints, agent session provenance, developer context, Git-native hosting/reviews | Optional minimal provenance; secrets, transcript size and public-ref propagation must be safer by default |
| Claude Code / Codex worktree use | Agent tasks run in isolated Git worktrees | No need to reproduce the agent runtime; support them through generic CLI/JSON |
| Conductor / Superset / Claude Squad | Manage many agent sessions and workspaces | Do not build a competing terminal multiplexer or GUI in V1 |
| Container Use / Docker Sandboxes | Containers for runtime/process isolation | Expose validation/sandbox interface; don't build Docker alternative |
| GitHub stacked PRs and merge queues | Gated/ordered integration | Preserve forge workflows where possible; not a novel merging concept |

### 2.3 Capabilities and evidence matrix

Legend: **Y** documented directly; **P** partial/adjacent mechanism; **E** via external tool/configuration; **?** not verified. This matrix is **not** a correctness benchmark. ARC column is a design target, not a shipped feature.

| Capability | Git | jj | GitButler | gitta | Entire | ARC target |
|---|---|---|---|---|---|---|
| CLI-first everyday VCS | Y | Y | Y | E (Git) | E (Git) | Y |
| Workspaces / concurrent file isolation | Y | Y | P | Y (worktrees) | E (Git) | Y |
| Operation-level undo | P (reflog, revert) | Y | Y | E | P | Y |
| First-class conflict objects | P (index stages) | Y | P | P | E | Y |
| GitHub-compatible Git remote | Y | Y | Y | Y | Y | Y (bridged) |
| Agent intent/dependency notices | E | E | P | Y | P | Y |
| Combined-state test gating | E | E | E | Y | P | Y |
| Cross-PC agent coordination | E | E | E | Y (enterprise) | P | Y |
| Integrated shared-branch CAS publication | Y (Git refs via remote) | E (Git remote) | E (Git remote) | E (Git) | E (Git) | Y with validation |
| Opt-in agent provenance | E | E | P | P | Y | Y minimal/opt-in |
| Runtime container isolation | E | E | E | P (Linux sandbox) | E | Integration interface, not bundled |

Do not market the cross-PC coordination row without running an apples-to-apples gitta Enterprise comparison.

## 3. User complaints and engineering risks

### P0 — correctness and critical product experience

1. **Agents overwrite each other's files in the same worktree.** Firsthand reports in ClaudeAI and AI_Agents Reddit discussions. Remedy: private workspaces; never a shared mutable checkout.
2. **Agents are not aware of incompatible changes elsewhere.** Worktrees isolate files but not decisions/API contracts. Remedy: published change events + declared intent + dependency analysis; advise before applying.
3. **Merge reports clean but combined app fails tests.** Remedy: test actual candidate snapshot, not each agent's private branch only; freeze exact candidate hash in test attestation.
4. **Concurrent publication races on the logical branch.** Remedy: immutable changes + authoritative `compare-and-swap(branch, expected_head, proposed_head)`; loser retries integration, never overwrite.
5. **Snapshot misses data.** Example: Jujutsu #10509 (mtime and size unchanged). Remedy: filesystem metadata may accelerate scans but must not be the only correctness oracle; verify content before destructive checkout; make all acknowledged snapshots durable.
6. **Operation undo affects wrong workspace / concurrency divergence.** Example: Jujutsu issues #10476 and #10480; design per-workspace operation ancestry, explicit selective reversions, operation concurrency tests.
7. **Merge/rewrite silently deletes or corrupts file content.** GitButler issue #15613. Remedy: three-way merge/replay invariants, object preservation, never mutate branch until integration is verified.
8. **Secret/private data in agent transcripts shared via repository.** Entire docs/issue #2951. Remedy: no transcript capture by default; provenance refs not advertised on public remotes; explicit opt-in + preview + encryption/permissions for sensitive metadata.
9. **Unsupported or malformed objects from untrusted peers.** Remedy: schema-version negotiation, hash verification, length limits, resource budgets, path validation and authorization.

### P1 — ergonomic and operational risks

10. Worktree-specific DBs, ports, dependencies, processes are not isolated by version control. Expose optional sandbox/runtime adapters with clear limitation; no guarantee of environment isolation without an adapter.
11. Exploding memory/disk from replayed, duplicated agent transcripts. Entire #2663 and #2979. Use capped incremental metadata events, compression, retention/GC, content dedup, quotas.
12. Hooks and plugins can conflict, including shell hooks overwritten or left active. Entire #2965 and #2960. Make agent adapters explicit, reversible, versioned and scoped.
13. Windows junction/symlink/rename semantics and case folding. Jujutsu #10521. Implement platform-specific secure checkout and test matrix.
14. Partial commits, staging mental overhead, stacked changes manipulation. Jujutsu/GitButler benchmark target: fewer commands, fewer agent tokens and fewer repair cycles.
15. Disconnected cross-device work, stale presence, network partitions and reconnect storms. Keep local operations available, distinguish local/published head, idempotent sync.
16. Conflict escalation and review overload. Emit actionable reports with provenance, impacted symbols and exact failing test; do not auto-approve unverified results.

### P2 — long-horizon parity

17. Multi-GB assets and binary conflicts; Git LFS pointer pattern is a precedent.
18. Large monorepos and sparse/partial clone.
19. Submodules, symlinks, file modes, rename detection and unusual filesystem encodings.
20. Multiple remotes, shallow history, tags, signed commits, Gerrit and protected branch policies.
21. Scale to large event histories; GC reachability, pack compaction and garbage-collection races.
22. Git round-trip compatibility and identities across SHA-1/SHA-256 object ID domains.
23. Credential security, audit, multi-tenant authorization and hostile remote servers.
24. Human onboarding, discoverability, configuration portability and minimal install size.

## 4. Product requirements (proposed)

**User types:** solo dev using CLI; human + 2–5 local agents; human/agents spread across Linux, Windows, macOS; CI worker; self-hosted team.

### Mandatory user journeys

1. `arc init`; edit, `arc status`, `arc diff`, `arc save -m "..."`, `arc log`, `arc undo`; **no agent required**.
2. `arc workspace create auth --branch main`; create independent workspace whose uncommitted files are not shared.
3. Two agents on same host publish independent immutable changes to the **same logical branch** without staging races.
4. PC A and PC B both join `main`; offline work creates local unpublished change heads; reconnect transfers all missing objects/events; only one `published/main` ref is authoritative.
5. Cross-PC update of an API symbol emits a **warning** to affected agents; no forced execution of instructions from another agent.
6. `arc integrate --check` builds a candidate tree, checks conflicts, executes policy-defined tests in configured isolation, returns immutable validation result.
7. `arc integrate --publish` updates the remote shared branch only if exact tested candidate and expected head match; else retry after new head.
8. `arc restore` and per-workspace `arc undo` do not destroy another actor's acknowledged changes.
9. `arc git export` pushes supported history to an existing Git remote; clearly describe unsupported semantics and preserve raw native metadata separately.
10. `arc doctor` diagnoses missing objects, stale refs, failed transactions, credential configuration and protocol mismatch.

### Non-goals for first public beta

- A full GitHub replacement, bespoke GUI, IDE, issue tracker, build system, agent orchestrator, LLM conflict solver, general CRDT editor, universal semantic-program correctness proof, Kubernetes runtime, or mandatory cloud service.

## 5. Fundamental model and terms

- **Blob:** immutable byte content; hash of canonical typed bytes. Store all file bytes verbatim.
- **Tree:** immutable directory representation including name, mode, entry type, content IDs. Canonical order/encoding.
- **Snapshot:** immutable tree + parent revision links + metadata; represents materialized code state.
- **Change:** independent logical change identity with immutable base and patch metadata. Its identity may be stable across applications; resulting snapshot IDs change as context changes. Do not promise all changes commute.
- **Operation:** append-only record of local/ref/workspace mutations, with parent operation IDs; separate from code snapshots.
- **Workspace:** private checkout, head, base, working-file scan state, agent identity optional.
- **Logical branch:** shared integration target (e.g. `main`), distinct from any workspace checkout.
- **Published ref:** authoritative remote CAS-protected branch head. Local branches may diverge offline.
- **Proposal:** immutable candidate of combining changes; can contain unresolved first-class conflicts; not necessarily publishable.
- **Validation attestation:** tested snapshot ID, exact test suite definition/version, environment image ID, execution status, timestamps, expiration policy and runner identity. Tests are evidence, never mathematical proof of behavior.
- **Intent event:** advisory declaration of files/symbols/task with expiry; not authoritative lock, cannot confer access.

Keep **ObjectID** (content-addressed), **ChangeID** (logical), **OperationID**, **WorkspaceID** and **DeviceID** distinct.

### Core invariants

`I1.` An ACKed object remains reachable or is retained in a defined recovery window and cannot disappear through GC while referenced.

`I2.` An acknowledged local operation is fsync-durable (within platform/filesystem contract) before success is reported.

`I3.` All visible published refs point to already-durable, hash-verified objects.

`I4.` Publication is serialized with CAS on `expected_head`, preventing silent lost updates.

`I5.` A failed/aborted integration leaves published ref unchanged.

`I6.` No conflicting candidate silently becomes published code; user/policy-approved resolutions are explicit changes.

`I7.` A validation result covers **exact candidate hash + gate configuration**, and is invalidated if the candidate/head changes.

`I8.` Undo defaults to the caller's workspace scope; remote/public undo needs explicit acknowledgment or a new forward change.

`I9.` Object parsing, checkout paths and permission decisions cannot trust remote metadata or agent instructions.

`I10.` All network sync operations are idempotent under duplicate, reordered and delayed messages.

`I11.` Offline edits are permitted; a global linear published ref requires an authoritative coordinator while connected. Under partition, local working heads can diverge.

`I12.` No silent uploading of agent transcripts; metadata sharing requires explicit policy and preview.

### Correctness note

“Branch remains one” means **one authoritative published head**, not that offline peers never diverge and not that the working directories are identical in real time. The eventual behavior is: independent private work → sync immutable changes → propose integration → validate → CAS-publish. Without coordination, simultaneous offline writes cannot both instantly become one linear published head.

## 6. Architecture

```text
Human / Claude / Codex / CI / other agent
                    |
         arc CLI (TTY + --json)
                    |
              ARC Core API
        ____________|____________________
       |            |          |        |
  Working copy   Changes    Operations  Policy
  & workspaces   & merge     log        engine
       |            |          |        |
       +------------+----------+--------+
                    |
         Native immutable object store
         + SQLite transactional metadata
                    |
           Authenticated sync protocol
          _________|_____________________
         |               |              |
   Local PC A        Remote relay     Local PC B
   private WSs       CAS ref head     private WSs
                           |
                 Git bridge / GitHub
```

### Engine and local storage

Use Rust workspace crates with explicit traits and stable internal APIs; build a simple safe object store first. A metadata store such as SQLite in WAL mode can coordinate local references and operations; object blobs remain immutable and can live outside SQLite. Use a **single-writer transaction protocol** for SQLite ref updates and crash recovery. Objects must be durable before a committed ref can name them. Document flush and fsync order; test `kill -9`, torn writes, power-loss simulation where feasible.

Candidate hashing: BLAKE3 with domain-separated tags and canonical serialization. This is **native ARC-only**; Git SHA-1/SHA-256 IDs are different. Exports must construct valid Git blobs/trees/commits, preserve identity mapping and report unsupported metadata, rather than pretending IDs are interchangeable.

Separate `arc-store` from `arc-git` behind traits. The Git bridge should be usable early so ARC can work with existing codebases; avoid claiming full round-trip parity until tested.

### Remote sync protocol v1

1. Authenticate client/device and repository authorization.
2. Exchange protocol version, capability flags, branch state, retention and object format.
3. Exchange summaries of locally available object IDs and heads.
4. Request missing reachable objects; transfer resumably with chunk checksums, length limits and compression.
5. Verify canonical IDs, graph closure and repository permissions before promotion from quarantine.
6. Replicate idempotent operation/change metadata and optional intent events.
7. Client proposes CAS publication `{branch, expected_head, candidate_id, attestation}`.
8. Relay validates authorization, object availability and policy; on success updates ref atomically and broadcasts event; otherwise returns observed head and conflict state.

**Authoritative relay** in early versions: a small self-hostable `arcd` process (authenticated HTTP/2 or QUIC later). A relay can be local/on a VPS. It is a coordinator, not the owner of source code in the sense of requiring a SaaS company. Full P2P ref consensus is a separate research project.

### Sync visibility levels

- `awareness`: optional ephemeral intent + agent presence event; never trusted as an instruction.
- `fetch`: make acknowledged remote changes available without mutating current workspace.
- `apply`: adapt changes into a private workspace, explicitly approved or policy-controlled.
- `publish`: tested, CAS-protected integration to shared branch.

Never silently replace active files due to a `sync --watch` notification.

### Merge policy

V1: robust file-level three-way textual merge, binary conflict reporting, first-class conflict objects and rename handling in a limited tested scope. Structural merge (Tree-sitter) is an **optional later strategy** with language-specific validity checks; AI can propose a resolution but must not be a correctness dependency. Always test merged result, not merely source workspaces.

### Security model

- Treat agents, hooks, tests, remotes, code and metadata as separate trust boundaries.
- Local scripts and test gates execute arbitrary repository code; explicitly request/define sandbox policy, never silently execute untrusted remote code.
- Authenticate and authorize branch publication; enforce protected refs and allowed runners.
- Block path traversal, symlink escapes, repo hijacking, pack decompression bombs, oversized metadata, event replay, spoofed agent identity.
- Use no mandatory remote agent transcription. Store only `agent_id`, task reference, change IDs, optional declared intent by default.
- Redaction is not sufficient for safe default public transcript uploads; explicit opt-in and destination review.
- Signed statements, tokens, mTLS, key rotation, retention controls and audit trails are staged features.

## 7. Proposed Rust workspace

```text
arc/
  Cargo.toml
  crates/
    arc-cli/          clap, output formats, stable exit codes
    arc-core/         typed domain model + validation rules
    arc-objects/      canonical encoding, hashing, immutable store
    arc-refs/         CAS branch ref and local workspace refs
    arc-ops/          journal, crash recovery, undo
    arc-workspace/    checkout, scan, isolation
    arc-diff/         file changes, text and binary handling
    arc-merge/        3-way merge, conflict objects
    arc-sync/         object negotiation, download/upload, reconnect
    arc-protocol/     versioned wire messages, auth scopes
    arc-server/       optional self-hosted relay
    arc-git/          Git import/export with gix/gitoxide as candidate
    arc-agent/        optional intent/events/adapters
    arc-validation/   isolated candidate execution and attestations
  tests/
    fixtures/ property/ crash/ concurrency/ multi-device/ interop/
  benches/
    storage/ diff/ checkout/ integration/ agent-experience/
  docs/
    rfcs/ adr/ protocols/ guarantees/ security/
```

Library candidates (pin versions only after checking compatibility): `clap`, `serde`, `postcard` or explicit canonical codec, `blake3`, `zstd`, `rusqlite`, `tokio`, `rustls`, `quinn` (later), `gix`, `tree-sitter`, `proptest`, `loom`, `cargo-fuzz`, `criterion`. **Serialization MUST be canonically specified before relying on generic serialization as part of a hash.**

Prefer a *single synchronous local engine* initially; network concurrency is introduced once invariants and crash consistency are tested. Do not prematurely turn the entire codebase into async tasks.

## 8. CLI contract (proposed only, not yet functional)

```bash
arc init
arc status --json
arc diff
arc save -m "implement token refresh"
arc log --graph
arc undo --workspace current
arc workspace create auth --branch main
arc workspace list
arc changes list --incoming
arc sync --fetch
arc sync --watch
arc agents status --json
arc integrate auth api --check
arc integrate auth api --publish
arc conflicts list --json
arc restore --snapshot <id>
arc verify --full
arc doctor
arc git import <url>
arc git export --remote origin
```

Errors are structured with stable machine-readable codes (`ARC_CONFLICT`, `ARC_STALE_HEAD`, `ARC_OBJECT_MISSING`, `ARC_POLICY_FAIL`, `ARC_AUTH_DENIED`, etc.). `--json` prints one versioned JSON document for bounded commands; `--jsonl` streams event records for `--watch`. Human output is useful independently. Avoid shell-output parsing as an agent protocol.

## 9. Roadmap (quality-gated, not a promise of calendar completion)

**Effort frame:** these are relative *sprint-sized estimates* for planning. Implementation time varies heavily with solo vs team work, testing depth, supported OSes and Git parity scope. A credible broad Git replacement requires substantially more work than a prototype.

### Stage R0 — RFC & competitive reproduction

**Goal:** establish specification and empirical baseline before writing irreversible disk format.

Deliverables: ADR-001 entity IDs/branch semantics; ADR-002 local vs published refs; ADR-003 native store vs Git bridge; ADR-004 metadata privacy; ADR-005 validation policy; acceptance criteria and threat model.

Reproduce at least eight scenarios in Git + jj + but + gitta where available: concurrent edits, shared API change, test-green individual/test-red merged, concurrent undo, simultaneous branch publish, offline reconnect, long transcript overhead, crash during snapshot.

**Gate:** an exact expected behavior is written for each case; prototype harness runs and stores reproducible fixtures; competitor-supported vs unsupported labeled correctly.

### Stage R1 — Native local VCS kernel (P0)

Features: `init`, immutable blobs/trees/snapshots, `status`, `diff`, `save`, `log`, `restore`, stable refs, `verify`, file mode/symlink rules, atomic metadata transactions and operation journal.

**Gate:** property-based tests show snapshot→restore round-trip of random trees; injection at each transaction boundary never exposes a ref pointing to missing objects; corrupt objects are detected, never accepted; deterministic tree hashing across OS fixture suite.

### Stage R2 — Private workspaces + concurrency (P0)

Features: N private workspaces associated with one logical branch; workspace-specific heads and op log; per-workspace undo; conflict objects; no shared index; atomic CAS; Git import/export **minimum useful subset** (ordinary files, commits, branches, tags where supported).

**Gate:** 2, 5 and 10 simultaneous local writers preserve all acknowledged changes, even during retries, forced termination and conflicting target head updates. Reopening after crash succeeds without manual object surgery.

### Stage R3 — Two-PC authenticated sync (core differentiation)

Features: minimal `arcd`, device keys or scoped tokens, head/ObjectID negotiation, resumable hash-verified transfer, `sync --fetch`, `--watch`, branch CAS publish, retry and divergence messages. Initially Linux↔Linux, then Windows↔Linux and macOS compatibility tests.

**Gate:** two agents independently modify same logical `main` across machines; disconnect both, work offline, reconnect in reversed orders; every acknowledged changeset exists on at least one authorized replica; only one published main head at any time on the relay; no silent overwrite; 100 retry/reordering trials succeed.

### Stage R4 — Verified integration (P0)

Features: deterministic three-way merge, first-class conflict storage, dry-run candidate, test-gate configuration, candidate hash-bound validation evidence, guarded publish with TOCTOU recheck, branch protections and explicit manual resolutions.

**Gate:** clean-text merge with broken combined tests is rejected; test result for old candidate cannot authorize a changed candidate; gate crash does not change published head; malicious/untrusted test execution is disabled or sandboxed per policy.

### Stage R5 — Agent-aware coordination (P1)

Features: optional `arc agent` registration, time-limited intent events, file/symbol-level warnings, `--json`/`--jsonl` stable schemas, real-time cross-PC notices; optional adapters for common agent CLIs without global-hook takeover.

**Gate:** cross-PC function signature change creates warning to dependent agent; false positive/negative rates measured over corpus; disconnected agents reconcile events without turning expired intent into a lock; no secrets silently emitted.

### Stage R6 — Developer beta and Git ecosystem (P1)

Features: quality UX, GitHub push/pull bridge, installer/release pipeline for Linux/macOS/Windows, `doctor`, clear errors, docs, CLI shell completion, config and migration tools, auth/permission hardening.

**Gate:** end-to-end trial on three real repositories with 2–5 agents and at least two devices; issue-driven fixes; no unresolved critical data-integrity/security bugs; documented export escape hatch; correctness comparison vs Git and jj.

### Stage R7 — Advanced VCS (P2; after beta evidence)

Potential investments: structural merge, rename moves, semantic dependency graph, partial clone/sparse checkout, chunked huge files, pack compaction/GC, signed change provenance, protected branch roles, merge queue integration, rebase-free commutative change models, additional remote transports.

**Gate:** each feature must beat simpler approach on explicit benchmark or user demand, while maintaining all P0 invariants.

## 10. Initial development sprint / actionable tasks

1. Write `docs/rfcs/0001-vcs-semantics.md`: exact definition of blob/tree/snapshot/change/operation/workspace/branch, and ID scheme.
2. Write `docs/rfcs/0002-durability-and-transactions.md`: crash ordering, fsync points, journal and quarantine.
3. Write `docs/rfcs/0003-sync-protocol.md`: object graph exchange, duplicate messages, expected-head CAS, offline divergences.
4. Create Rust workspace skeleton `arc-cli`, `arc-core`, `arc-objects`, `arc-refs`, `arc-ops` and integration-test harness.
5. Implement canonical blob hashing and object-store write/read with corruption detection.
6. Implement typed tree hashing with path validation, permissions/symlink behavior and cross-platform fixture hashing.
7. Implement journaled `save` transaction with failure injection at each write/rename/fsync boundary.
8. Add randomized snapshot↔restore tests and reference preservation properties.
9. Publish reproducible baseline results with Git/jj/but/gitta (environment and versions pinned).
10. Do not implement semantic AI merge, cloud dashboard or agent runtime before these correctness gates pass.

## 11. Benchmark design and acceptance metrics

### Correctness (hard gate)

- **0 lost acknowledged changes** in test matrix; continuous fuzz and failure-injection corpus.
- **0 accepted corrupt object chains**; all snapshots and incoming objects hash checked.
- **0 stale-head publishes** (CAS conflicts must fail safe and retry).
- **0 validated-test/hash mismatches**; no bypass of branch policy.
- **0 silent private transcript uploads** by default.

### Usability / agent ergonomics (comparative, not invented benchmarks)

- Measure version-control command count, tool-token count, wall-clock task completion, retries and success for the **same exact tasks and agent models** across Git, jj, but, ARC.
- Collect humans' task times for undo, inspect, checkout recovery, select changes, cross-PC sync.
- Define parity threshold from baseline before setting numeric targets; avoid promising 65% speed improvements without independent tests.

### Performance/operability

- p50/p95 cold and warm `status`, commit/save, diff, workspace create, object transfer and integrate under small, medium, large test repos.
- Peak RAM/disk and compression ratios; 10 concurrent writers; 2 and 3 computers; 1%–20% simulated packet loss; 0/100/500ms latency profiles.
- Cold-install single-binary size, first-run time and per-command latency; compare full clone vs filtered clone later.
- 24h concurrency soak, 72h sync soak in beta; rotating random power-off and network partition fault injections.

All quantitative targets to be calibrated by initial baselines. “No data loss” cannot be established universally by finite tests; the goal is to back the guarantee with strict invariants, static review, audits and adversarial testing.

## 12. Go/no-go criteria and risk register

| Risk | Severity | Mitigation | Go/no-go rule |
|---|---|---|---|
| Independent storage format consumes all development effort | High | `Backend` trait; Git bridge early; small minimum format | If Git interop remains unusable by R2, narrow beta positioning |
| Cross-device single-head semantics misunderstood | Critical | Explicit local vs published state + CAS relay | No public beta without partition/rejoin tests |
| Snapshot/GC/corruption data loss | Critical | Durable ACK contract; fuzz + crash injection; immutable retention | No beta with reproducible acknowledged-loss bugs |
| AI-generated merges are semantically wrong | Critical | No AI trust-by-default; tests on candidate, manual approval | No auto-publish from LLM-only resolution |
| Privacy/supply-chain issues with agent hooks | Critical | No transcript capture default; scoped adapters; sandbox policy | No automatic public metadata upload |
| Product indistinguishable from gitta/jj/but | High | Empirical 2-PC benchmark + UX comparison | Revisit architecture if no measurable use-case advantage |
| Performance worse than Git on ordinary use | Medium | profiling, benchmarks, pack/GC plans | Define explicit degradation budget and narrow beta |
| Over-expanding to GUI/hosting/orchestration | High | Explicit non-goals | Reject out-of-scope backlog |

## 13. Open architecture decisions

1. **Storage:** independent BLAKE3 native store from V1 vs Git-format storage plus native metadata? Recommendation: design a storage trait and keep ARC-native semantics, but implement useful Git bridge early. Prove round-trip coverage rather than assuming it.
2. **Change algebra:** snapshot+identity+patch representation for MVP; delay full commuting patch theory until experimentally justified.
3. **Operations scope:** per-workspace default undo, explicit shared-ref restoration; verify event DAG semantics under concurrency.
4. **Agent presence:** ephemeral and advisory; never implicitly branch-authorizing. Decide TTL and replay semantics.
5. **Validation:** runner execution permission model, deterministic environment fingerprints, gate expiration, branch protection.
6. **Metadata privacy:** whether agent identity itself counts as sensitive in team repositories; document threat model.
7. **Remotes:** `arcd` authoritative reference server + Git compatibility bridge vs direct Git remote as initial coordinator. Two transport adapters may share one branch semantics layer.
8. **Naming/license:** `arc` is placeholder; check executable collisions, domain/trademark, project license and GitButler license restrictions before shipping or reusing code.

## 14. Sources (retrieved in the 2026-11-01 research session)

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

---

**Next implementation artifact:** RFC 0001 data model + format versioning; then Rust workspace skeleton and initial correctness tests. Do not mark any milestone complete until its gate passes.
