# concurv roadmap status

Legend: `not started` · `in progress` · `gate passed` · `blocked`.
A stage is `gate passed` only with the evidence listed next to it.
Last updated: 2026-11-01.

| Stage | Status | Evidence / blockers |
|---|---|---|
| R0 — RFC & competitive reproduction | **in progress** | Done: RFC 0001–0003, ADR 0001–0002, threat model, comparison plan with exact expected behaviour for S1–S10. **Open gate items:** competitor harness (`bench/scenarios/`) not built; **no** Git/jj/but/gitta/Fossil/Pijul scenario reproduced (only Git is installed on the dev machine). |
| R1 — Native local VCS kernel | **in progress — all gate checks pass on Windows; Linux/macOS run outstanding** | See evidence below. Blocker for `gate passed`: the gate requires deterministic hashing across an OS fixture suite and the Unix-only tests (symlinks, exec bits, FIFO, non-UTF-8 names, symlink-escape restore) have **not been executed** — no Linux/macOS machine or CI was available. |
| R2 — Private workspaces + concurrency | not started | design constraints in RFC 0001 §7 |
| R3 — Two-PC authenticated sync | not started | design only: RFC 0003 |
| R4 — Verified integration | not started | — |
| R5 — Agent-aware coordination | not started | — |
| R6 — Developer beta + Git ecosystem | not started | design only: ADR 0002 |
| R7 — Advanced VCS | not started | — |

## R1 evidence (Windows 11 Home 10.0.26300, NTFS, rustc 1.95.0)

| Command | Result |
|---|---|
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo test --workspace --all-targets` | **59 passed, 0 failed, 0 ignored** |
| `cargo build --release` | pass (`target/release/concurv.exe`) |

Gate item → tests:

| R1 gate requirement | Tests |
|---|---|
| Property-based snapshot→restore round-trip of random trees | `concurv-workspace/tests/roundtrip_props.rs` (48 cases/run), `concurv-objects/tests/objects.rs` proptests (encoding canonicality, decoder totality, bit-flip) |
| Fault injection at each transaction boundary never exposes a ref to missing objects | `concurv-workspace/tests/fault_injection.rs`: save 72 cases (6 failpoints × every hit, incl. torn writes), init 38 cases; `restore_safety.rs::interrupted_restore_is_reported_and_resumable` fails every rename of a restore |
| Corrupt objects detected, never accepted | `objects.rs::corruption_is_detected_never_accepted`, `oversized_headers_*`, `engine.rs::corrupt_reachable_object_*`, `missing_snapshot_member_*`, `restore_safety.rs::corrupt_blob_in_target_*`, CLI exit 30/31 |
| Deterministic tree hashing across OS fixture suite | golden vectors `objects.rs::golden_hash_vectors`, `engine.rs::deterministic_tree_ids_across_repositories` — **verified on Windows only** |
| Safe restore (dirty, traversal, symlink escape, encodings) | `restore_safety.rs` (7 tests; symlink-escape test is Unix-only, not run), Windows names/junction/surrogate tests (run) |
| CLI end-to-end | `concurv-cli/tests/cli_smoke.rs` |

Tests compiled only on Unix and therefore **not run**: `unix_exec_bits_and_symlinks_roundtrip`,
`unix_fifo_and_non_utf8_names_are_rejected`, `symlinked_directory_is_replaced_not_written_through`.

Fuzzing: not configured. Benchmarks: none run; no performance numbers exist.
