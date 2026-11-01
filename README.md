# concurv

**concurv** stands for *concurrent, agent-native version control*: a local-first
VCS in Rust. The goal is for humans and AI agents on different machines to
contribute to one shared branch without ever silently losing acknowledged work
(see `docs/ARC_Research_and_Roadmap_2026.md`; "ARC" was the project's earlier
working name).

**Current state: R1 local kernel.** One workspace on one machine. There is no
sync, merge, undo, multiple-workspace support or Git bridge yet. Trademark and
package-name availability for `concurv` have not been checked.

## Quick start

```console
$ cargo install --path crates/concurv-cli
$ cd my-project
$ concurv init
$ echo hello > notes.txt
$ concurv status                 # read-only, compares file contents
$ concurv save -m "first"        # durable snapshot
$ echo changed > notes.txt
$ concurv diff                   # unified diff against the head
$ concurv save -m "second"
$ concurv log
$ echo unsaved > notes.txt
$ concurv restore --snapshot <first-id-prefix>          # refused (exit 20); nothing touched
$ concurv restore --snapshot <first-id-prefix> --force  # saves a backup snapshot, then restores
$ concurv op log                 # every operation, including backup snapshots
$ concurv verify --full          # hash-checks every object
$ concurv --json status          # machine-readable output
```

`restore` never rewrites history. It only changes working files, and
`concurv save` then records the restored state as a new snapshot.
Full walkthrough and recovery procedures: **`docs/user-guide.md`**.

## Guarantees and limits

- When `save` succeeds, every object and the new operation have been fsynced,
  and the repository's single head pointer has been replaced atomically
  (RFC 0002). On Windows, directory fsync isn't available, so rename
  durability relies on NTFS journaling.
- Corrupt, truncated, wrong-kind or missing objects are always detected and
  never accepted. `restore` verifies the whole target before touching any file.
- Rejected rather than silently skipped: symlinks and junctions on Windows,
  non-UTF-8 file names, FIFOs and devices.
- Not implemented yet: ignore files, rename detection, garbage collection
  or a stat cache (every `status` reads all files).
- Don't keep repositories in cloud-synced folders (OneDrive, Dropbox).

## Build and test

```console
$ cargo build --release                    # target/release/concurv
$ cargo test --workspace --all-targets
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo fmt --all -- --check
```

Workspace layout: `crates/concurv-{core,objects,refs,ops,workspace,cli}`
(see `docs/architecture.md`).

## Documentation

| Document | Contents |
|---|---|
| `docs/user-guide.md` | install, workflow, restore and backups, troubleshooting, platform notes |
| `docs/cli.md` | CLI contract: commands, JSON schema, error and exit codes |
| `docs/architecture.md` | crate map, key design decisions, `save` data flow |
| `docs/ROADMAP_STATUS.md` | stage status R0–R7 with test evidence |
| `docs/rfcs/0001-vcs-semantics.md` | data model, normative byte format, path rules, test vectors |
| `docs/rfcs/0002-durability-and-transactions.md` | write ordering, fsync, commit point, recovery |
| `docs/rfcs/0003-sync-protocol.md` | future multi-device sync (design only) |
| `docs/adr/0001-storage-format.md` | why a native BLAKE3 store with one mutable pointer |
| `docs/adr/0002-git-interop.md` | future Git bridge boundary |
| `docs/adr/0003-project-rename-concurv.md` | the ARC → concurv rename and every identifier it changed |
| `docs/security/threat-model.md` | threats and mitigation status |
| `docs/benchmarks/comparison-plan.md` | competitor reproduction plan (no results yet) |
| `CHANGELOG.md` | changes since the start of the project |
| `CLAUDE.md` | rules for AI coding sessions working on this repo |
