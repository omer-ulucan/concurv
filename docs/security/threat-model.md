# ARC threat model

Scope: R1 local kernel (implemented) and R3–R5 surfaces (designed). Each row
lists the mitigation **and whether it is implemented**.

## Assets

Acknowledged history (objects, operations), uncommitted working files,
files outside the repository, credentials/tokens, agent transcripts and
private metadata, published branch heads (R3).

## Adversaries and mitigations

| # | Threat | Mitigation | Status |
|---|---|---|---|
| T1 | **Local attacker / buggy tool corrupts objects** (bit flips, truncation, edits in `.arc/objects`) | every read re-hashes; `arc verify --full`; corrupt dedup targets quarantined; fail closed | R1 implemented + tested |
| T2 | **Hostile object graph** (malformed headers, huge lengths, unsorted/duplicate entries, cycles, wrong kinds) | strict decoder, size limits before allocation, typed loads, kind check; trees are hash-linked so cycles are impossible without a hash collision | R1 implemented + tested |
| T3 | **Path traversal via tree names** (`..`, `/`, `\`, absolute, NUL, `.arc`) | name validation on encode *and* decode; restore re-validates | R1 implemented + tested |
| T4 | **Symlink escape** (symlink in workspace makes restore write outside root; tree with `a` symlink then `a/b` file) | restore never follows symlinks: each parent checked with `symlink_metadata`; existing symlinks are removed, not written through; symlink targets stored as data only | R1 implemented; Unix symlink tests run only on Unix CI (not run on the Windows dev machine) |
| T5 | **Windows-specific names** (`CON`, `aux.txt`, `a:b` ADS, trailing dot/space, case collisions) | refused before any write on Windows; case-collision check on Windows/macOS | R1 implemented; tested on Windows |
| T6 | **Untrusted checkout path** (repository root inside a symlinked dir, `.arc` replaced by symlink) | `.arc` must be a real directory (checked with `symlink_metadata`) | R1 implemented |
| T7 | **Concurrent local writers** | OS exclusive lock + CAS on op_head | R1 implemented + tested |
| T8 | **Dirty working copy destroyed by restore** | refuse without `--force`; `--force` makes durable backup snapshot first; content re-check just before each replace | R1 implemented + tested; residual TOCTOU documented |
| T9 | **Malicious remote** (lies about heads, sends bad objects, replays publishes) | RFC 0003: auth, quarantine, closure check, idempotent ids, CAS on relay, request ids | R3 design only |
| T10 | **Network replay / reorder** | content-addressed messages, request ids, TTL for events | R3/R5 design only |
| T11 | **Transcript / secret leaks** | no transcript capture at all in R1; future provenance opt-in, local-only by default, preview before publish | R1: nothing captured; author string is the only identity stored (from `ARC_AUTHOR` or OS user name) |
| T12 | **Arbitrary test code during validation** | R4: validation disabled unless policy enables it; explicit sandbox adapter; attestation records the environment | R4 design only; R1 executes no repository code |
| T13 | **Resource exhaustion** (huge files, deep trees) | limits in RFC 0001 §5; streaming blob IO; recursion depth bounded by OS path length | R1 partially (no depth/total-size quota yet) |

## Out of scope

Compromised OS/kernel, hardware ignoring flushes, attackers with write access
to `.arc` who also forge consistent hashes for their own content (content
addressing provides integrity, not authenticity; signatures are R7).
