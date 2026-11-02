# concurv

[![ci](https://github.com/omer-ulucan/concurv/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/omer-ulucan/concurv/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/omer-ulucan/concurv?include_prereleases&sort=semver)](https://github.com/omer-ulucan/concurv/releases)

**concurv** stands for *concurrent, agent-native version control*: a local-first
VCS in Rust. The goal is for humans and AI agents on different machines to
contribute to one shared branch without ever silently losing acknowledged work
(see `docs/ROADMAP_REVISED.md`; "ARC" was the project's earlier
working name).

**Current state:** first pre-release, **v0.1.0-alpha.1**
([download](https://github.com/omer-ulucan/concurv/releases/tag/v0.1.0-alpha.1)).
Gates passed: R1 (local kernel), R1.5 (daily usability and safety) and R2
(workspaces and concurrency). R2.5 (local merge) is in progress. On one
machine: several private workspaces sharing one `main`, compare-and-swap
publication, three-way merge with line-level merging of non-overlapping
edits, recorded conflicts that you resolve and save, workspace-scoped
`undo`, Git-compatible ignores, and Git export/import. Stress-tested with
10 concurrent writers and killed processes, on Linux, macOS and Windows in
CI; see `docs/ROADMAP_STATUS.md`. **Not yet:** cross-machine sync (R3),
verified integration (R4), agent coordination (R5).

> **Alpha:** the on-disk format and the CLI/JSON contract may still change
> before 0.1.0. **No license has been chosen yet**: until one is added, the
> code and binaries are published for evaluation only, with no license granted.

## Install

Download the archive for your platform from the
[latest release](https://github.com/omer-ulucan/concurv/releases) (Linux
x86_64, macOS Apple Silicon and Intel, Windows x86_64), check it against
`SHA256SUMS`, and put `concurv` on your `PATH`. Or build from source with
Rust 1.89+:

```console
$ cargo install --git https://github.com/omer-ulucan/concurv --tag v0.1.0-alpha.1 concurv-cli
```

## Quick start

```console
$ cd my-project
$ concurv init
$ echo hello > notes.txt
$ concurv status                 # read-only, compares file contents
$ concurv save -m "first"        # durable snapshot
$ echo changed > notes.txt
$ concurv diff                   # unified diff against the head
$ concurv save -m "second"
$ concurv publish                # main := this workspace's head (save alone never publishes)
$ concurv log
$ echo unsaved > notes.txt
$ concurv restore --snapshot <first-id-prefix>          # refused (exit 20); nothing touched
$ concurv restore --snapshot <first-id-prefix> --force  # saves a backup snapshot, then restores
$ concurv op log                 # every operation, including backup snapshots
$ concurv undo                   # reverse this workspace's last save/update/restore
$ concurv verify --full          # hash-checks every object
$ concurv --json status          # machine-readable output
```

Parallel work, one workspace per person or agent:

```console
$ concurv workspace add ../my-project-agent   # private checkout of main
$ cd ../my-project-agent && echo work > agent.txt
$ concurv save -m "agent work"
$ concurv update                 # merge what others published (files, and non-overlapping edits within a file)
$ concurv publish                # exit 42 if main moved again: update and retry
```

Share with Git / GitHub (needs `git` on PATH):

```console
$ git init --bare ../my-project-mirror.git
$ concurv git export ../my-project-mirror.git   # main's history as Git commits
$ concurv git import ../some-git-repo          # (in a fresh repo) start from a Git branch
```

`restore` never rewrites history. It only changes working files, and
`concurv save` then records the restored state as a new snapshot.
Full walkthrough and recovery procedures: **`docs/user-guide.md`**.

## Guarantees and limits

- When `save` or `publish` succeeds, every object and the new operation have been fsynced,
  and the repository's single head pointer has been replaced atomically
  (RFC 0002). On Windows, directory fsync isn't available, so rename
  durability relies on NTFS journaling.
- Corrupt, truncated, wrong-kind or missing objects are always detected and
  never accepted. `restore` verifies the whole target before touching any file.
- Rejected rather than silently skipped: symlinks and junctions on Windows,
  non-UTF-8 file names, FIFOs and devices.
- Concurrent commands in different workspaces never overwrite each other's
  acknowledged work; `main` moves only forward, by compare-and-swap.
- `update` merges different files, and edits to different parts of one text
  file (at least one unchanged line between them, ADR 0006). Overlapping
  edits, binary files and symlinks are refused (nothing modified) or, with
  `--allow-conflicts`, written into the working copy for you to resolve.
  They are never guessed.
- Ignores work like Git (`.gitignore`, `.concurv/exclude`, `check-ignore`); ignored
  files are never overwritten. `.git/` is never versioned.
- `status`/`diff` use a stat cache on Linux and macOS (never for snapshots,
  ADR 0005; `--no-cache` to hash everything). Windows always hashes.
  Measurements: `bench/RESULTS.md`.
- Not implemented yet: rename detection, garbage collection, tags, symlinks
  on Windows.
- Don't keep repositories in cloud-synced folders (OneDrive, Dropbox).

## Build and test

```console
$ cargo build --release                    # target/release/concurv
$ cargo test --workspace --all-targets
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo fmt --all -- --check
```

Workspace layout: `crates/concurv-{core,objects,refs,ops,workspace,git,cli,bench}`
(see `docs/architecture.md`).

## Documentation

| Document | Contents |
|---|---|
| `docs/user-guide.md` | install, workflow, restore and backups, troubleshooting, platform notes |
| `docs/cli.md` | CLI contract: commands, JSON schema, error and exit codes |
| `docs/architecture.md` | crate map, key design decisions, `save` data flow |
| `docs/ROADMAP_REVISED.md` | the plan: stages R0–R7 (incl. R1.5, R2.5), gates, backlog |
| `docs/ROADMAP_STATUS.md` | stage status R0–R7 with test evidence |
| `docs/rfcs/0001-vcs-semantics.md` | data model, normative byte format, path rules, test vectors |
| `docs/rfcs/0002-durability-and-transactions.md` | write ordering, fsync, commit point, recovery |
| `docs/rfcs/0003-sync-protocol.md` | future multi-device sync (design only) |
| `docs/rfcs/0004-workspaces-and-concurrency.md` | workspaces, save vs publish, update and merge, locks and concurrency |
| `docs/adr/0001-storage-format.md` | why a native BLAKE3 store with one mutable pointer |
| `docs/adr/0002-git-interop.md` | the Git bridge: how export/import work, what is and isn't preserved |
| `docs/adr/0003-project-rename-concurv.md` | the ARC → concurv rename and every identifier it changed |
| `docs/adr/0004-ignore-rules.md` | Git-compatible ignores and the tracked-file model |
| `docs/adr/0005-stat-cache.md` | the stat cache and why it can never produce a wrong snapshot |
| `docs/adr/0006-hunk-merge.md` | line-level merge in `update` |
| `docs/security/threat-model.md` | threats and mitigation status |
| `docs/benchmarks/comparison-plan.md` | competitor reproduction plan (no results yet) |
| `bench/RESULTS.md` | `status`/`save` benchmark results (reproducible with `concurv-bench`) |
| `docs/releases/` | release notes |
| `CHANGELOG.md` | changes since the start of the project |
