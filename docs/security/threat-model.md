# concurv threat model

Scope: R1 local kernel (implemented) and R3–R5 surfaces (designed). Each row
lists the mitigation **and whether it is implemented**.

## Assets

Acknowledged history (objects, operations), uncommitted working files,
files outside the repository, credentials/tokens, agent transcripts and
private metadata, published branch heads (R3).

## Adversaries and mitigations

| # | Threat | Mitigation | Status |
|---|---|---|---|
| T1 | **Local attacker / buggy tool corrupts objects** (bit flips, truncation, edits in `.concurv/objects`) | every read re-hashes; `concurv verify --full`; corrupt dedup targets quarantined; fail closed | R1 implemented + tested |
| T2 | **Hostile object graph** (malformed headers, huge lengths, unsorted/duplicate entries, cycles, wrong kinds) | strict decoder, size limits before allocation, typed loads, kind check; trees are hash-linked so cycles are impossible without a hash collision | R1 implemented + tested |
| T3 | **Path traversal via tree names** (`..`, `/`, `\`, absolute, NUL, `.concurv`) | name validation on encode *and* decode; restore re-validates | R1 implemented + tested |
| T4 | **Symlink escape** (symlink in workspace makes restore write outside root; tree with `a` symlink then `a/b` file) | restore never follows symlinks: each parent checked with `symlink_metadata`; existing symlinks are removed, not written through; symlink targets stored as data only | R1 implemented; Unix symlink tests run only on Unix CI (not run on the Windows dev machine) |
| T5 | **Windows-specific names** (`CON`, `aux.txt`, `a:b` ADS, trailing dot/space, case collisions) | refused before any write on Windows; case-collision check on Windows/macOS | R1 implemented; tested on Windows |
| T6 | **Untrusted checkout path** (repository root inside a symlinked dir, `.concurv` replaced by symlink) | `.concurv` must be a real directory (checked with `symlink_metadata`) | R1 implemented |
| T7 | **Concurrent local writers** (many workspaces, agents and processes; processes killed mid-command) | workspace lock + commit-only repository lock with bounded waits; CAS on op_head against the current view; lock-free immutable object writes in per-process temp sessions that recovery never deletes while their owner lives; `main` moves only by fast-forward publish | R2 slice 2a implemented; tested with 2/5/10 concurrent writers and 12 rounds of killed processes on Linux, macOS and Windows |
| T7b | **One agent's workspace overwritten by another** | commands only change files of the workspace they run in; nested workspaces refused; merges happen in the private workspace, conflicts refused | R2 slice 2a implemented + tested |
| T8 | **Dirty working copy destroyed by restore** | refuse without `--force`; `--force` makes durable backup snapshot first; content re-check just before each replace | R1 implemented + tested; residual TOCTOU documented |
| T9 | **Malicious remote** (lies about heads, sends bad objects, replays publishes) | RFC 0003: auth, quarantine, closure check, idempotent ids, CAS on relay, request ids | R3 design only |
| T10 | **Network replay / reorder** | content-addressed messages, request ids, TTL for events | R3/R5 design only |
| T11 | **Transcript / secret leaks** | no transcript capture at all in R1; future provenance opt-in, local-only by default, preview before publish | R1: nothing captured; author string is the only identity stored (from `CONCURV_AUTHOR` or OS user name) |
| T12 | **Arbitrary test code during validation** | R4: validation disabled unless policy enables it; explicit sandbox adapter; attestation records the environment | R4 design only; R1 executes no repository code |
| T13 | **Resource exhaustion** (huge files, deep trees) | limits in RFC 0001 §5; streaming blob IO; recursion depth bounded by OS path length | R1 partially (no depth/total-size quota yet) |
| T14 | **Untrusted Git repositories and the `git` tool** (hostile paths or entries in an imported Git history; a `git` from `PATH`; environment variables redirecting git) | import validates every path with the same rules as `save` (`..`, `.concurv`, invalid names refused) and refuses submodules and non-UTF-8 data, all-or-nothing; blobs are re-hashed into concurv objects; `GIT_DIR`, `GIT_WORK_TREE` and similar variables are scrubbed and every call uses `git -C <repo>`; export never moves a Git branch except by fast-forward CAS. concurv trusts the `git` binary found on `PATH` (same trust as running `git` yourself) | R2 slice 2d implemented + tested |
| T15 | **Ignored local files and colocated Git metadata** (an ignored `.env` or build cache overwritten by restore/update; `.git/` captured into history; a snapshot planting `.git/hooks` on restore) | ignored untracked files are never overwritten or deleted (refusal even with `--force`); `.git` never scanned; nested repositories skipped; restore refuses `.git` entries (and `GIT~n` on Windows). Ignore rules are **not** a secrets boundary: already-saved secrets stay in history (redaction is future work) | R1.5 implemented + tested (ADR 0004) |

## Out of scope

Compromised OS/kernel, hardware ignoring flushes, attackers with write access
to `.concurv` who also forge consistent hashes for their own content (content
addressing provides integrity, not authenticity; signatures are R7).
