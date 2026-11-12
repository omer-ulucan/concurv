# Benchmark results (R1.5, P0-6)

Harness: `crates/concurv-bench` (deterministic fixtures from a fixed seed, no
network, no external tools). Reproduce with:

```
cargo run --release -p concurv-bench -- --sizes 100,1000,10000 --runs 10
```

Fixture with `n` files: nested `src/` tree, 90 % text files of 1–4 KiB and 10 %
binary files of 64 KiB, plus an ignored `target/` tree of `n/2` files that the
scanner has to skip. `status` runs on a clean tree; "no-op save" is `save`
with nothing changed. Wall-clock, warm OS cache; p50/p95 over 10 `status` runs
and 5 no-op saves.

Environment: Windows 11 Home (10.0.26300), NTFS on SSD, Intel Core
i7-13700HX, 16 GiB RAM, rustc 1.95.0, release profile. The tree lives inside
`%TEMP%`, outside OneDrive. Numbers from one machine are a baseline, not a
claim about other systems.

## Before: blob written and fsynced before the dedup check

| files | tracked | size | initial save ms | status p50 | status p95 | no-op save p50 | no-op save p95 |
|---|---|---|---|---|---|---|---|
| 100 | 101 | 0.8 MiB | 1171 | 23.8 | 36.8 | 341 | 357 |
| 1000 | 1001 | 8.4 MiB | 4813 | 92.6 | 117.4 | 3457 | 3510 |
| 10000 | 10001 | 84.5 MiB | 44833 | 844 | 967 | 46031 | 48326 |

The no-op save was as slow as the initial save. `save` streamed every file
into a temp file and fsynced it, and only then found the object already
stored.

## After: hash first, write only missing or corrupt objects

| files | tracked | size | initial save ms | status p50 | status p95 | no-op save p50 | no-op save p95 |
|---|---|---|---|---|---|---|---|
| 100 | 101 | 0.8 MiB | 1051 | 23.3 | 41.5 | 50.8 | 53.2 |
| 1000 | 1001 | 8.4 MiB | 4943 | 118.2 | 239.3 | 387.5 | 453.2 |
| 10000 | 10001 | 84.5 MiB | 37108 | 822.8 | 926.6 | 1702.9 | 1704.5 |

At 10k files the no-op save is about 27× faster (46.0 s → 1.7 s). An existing
object is still fully re-verified before it is trusted (`ObjectStore::has_valid`),
so a corrupt copy is quarantined and rewritten exactly as before (the self-heal
tests still pass). If a file changes between the hash pass and the write,
`write_blob_stream` returns the id of the bytes it actually stored, so the
snapshot is never mislabelled.

The initial save, about 3.7 ms per file, is dominated by per-object fsync
(RFC 0002). Batching fsyncs is a possible future change and needs its own
durability review.

## Stat cache (P0-7): deferred after measuring

A clean `status` on 10k files / 84.5 MiB takes 0.82 s (p50) with full content
hashing on every run. Under the V2 rules a stat cache is added only when
measurement shows it is needed, and it must never be the authority for a
snapshot. On the platform measured here (Windows) a safe cache cannot be
built anyway: the Rust standard library exposes no change time there and NTFS ChangeTime can be set by user code, so the
same-size, same-mtime rewrite case cannot be detected. A Unix-only cache
(ctime + inode + size + mtime, with a racy-timestamp guard) stays on the
roadmap as P0-7. It will be built when Linux/macOS measurements on larger
trees (≥100k files) show `status` is too slow, together with the false-clean
test suite the roadmap requires.

## Overlap warnings (R5, RFC 0006 §4)

`cargo test -p concurv-workspace --test overlap_rates -- --nocapture`;
seeded corpus, deterministic (same numbers on every machine). Recorded
2026-11-12, Windows 11 and WSL 2 (identical output).

**Conflict warnings**: 2000 pairs of random edits (replace, insert or
delete 1–3 lines) of one 60-line file; 1 in 20 pairs make the same edit on
both sides. Truth: the real three-way merge (`textmerge::merge3`, ADR 0006)
conflicts.

| Level | Real conflicts | False positives | False negatives |
|---|---|---|---|
| `path` (same file) | 105 of 2000 | 1895 of 1895 clean pairs (rate 1.000) | 0 (rate 0.000) |
| `conflict` (line ranges overlap or touch) | 105 of 2000 | 102 of 1895 (rate 0.054) | 0 (rate 0.000) |

The `conflict` warnings never miss a real conflict; their false positives
are pairs that make the same edit (which merges cleanly).

**Dependency warnings**: 2000 cases. Upstream changes either the signature
or only the body of one of 20 functions; the dependent change refers to a
function by a direct call, an aliased import, a comment, a string, or a
call through a function table (about 400 each). Truth: the dependent code
really calls the function whose signature changed (299 cases).

| Rule | False positives | False negatives |
|---|---|---|
| changed definition lines × uses in code (comments and strings ignored) | 0 of 1701 (rate 0.000) | 109 of 299 (rate 0.365) |

All false negatives are calls through a table: a lexical rule cannot see
them (RFC 0006 §5). Direct and aliased calls are always caught; a
body-only change never warns.
