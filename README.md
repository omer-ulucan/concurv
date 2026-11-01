# concurv

concurv — concurrent, agent-native version control. A local-first VCS in Rust, designed to grow into
agent-aware multi-device collaboration (see
`docs/ARC_Research_and_Roadmap_2026.md`). **Current state: R1 local kernel.**
There is no sync, merge, undo, multi-workspace or Git bridge yet.
Trademark and package-name availability for `concurv` have not been checked.

## What works today

```console
$ concurv init
$ echo hello > notes.txt
$ concurv status                 # read-only; content-hash based
$ concurv save -m "first"        # durable snapshot (fsync + atomic op-head swap)
$ echo changed > notes.txt
$ concurv diff                   # unified diff vs head
$ concurv save -m "second"
$ concurv log
$ echo unsaved > notes.txt
$ concurv restore --snapshot <first-id-prefix>          # refused: exit 20, nothing touched
$ concurv restore --snapshot <first-id-prefix> --force  # backup snapshot first, then restore
$ concurv verify --full          # hash-checks every object
$ concurv --json status          # versioned machine-readable output
```

`restore` never moves history: it rewrites working files only; `concurv save`
records the restored state as a new forward snapshot. CLI contract, JSON schema
and exit codes: `docs/cli.md`.

## Build and test

```
cargo build --release        # binary: target/release/concurv
cargo test --workspace --all-targets
```

## Guarantees and limits (short version)

- A successful `save` means all objects and the new operation were fsynced and
  the op head was atomically replaced (RFC 0002). Windows: directory fsync is
  not available, rename durability relies on NTFS journaling.
- Corrupt, truncated, wrong-kind or missing objects are detected and never
  accepted; restore verifies the whole target before touching any file.
- Not supported yet: symlinks/junctions **on Windows** (rejected), non-UTF-8
  file names (rejected), FIFOs/devices (rejected), ignore files, rename
  detection, garbage collection, stat cache (every `status` hashes all files).
- Do not put repositories in cloud-synced folders (OneDrive, Dropbox): their
  rename/flush semantics are not part of the durability contract.

## Documents

- `docs/ROADMAP_STATUS.md` — stage status with evidence
- `docs/rfcs/` — semantics & byte format, durability, sync (design only)
- `docs/adr/` — storage format, Git interop
- `docs/security/threat-model.md`, `docs/architecture.md`, `docs/benchmarks/comparison-plan.md`
- `prompts/START_WITH_CLAUDE_CODE.md` — engineering instructions used to build this
