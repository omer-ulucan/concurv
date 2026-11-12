# concurv

[![release](https://img.shields.io/github/v/release/omer-ulucan/concurv?include_prereleases&sort=semver)](https://github.com/omer-ulucan/concurv/releases)

**concurv** stands for *concurrent, agent-native version control*: a local-first
VCS in Rust. The goal is for humans and AI agents on different machines to
contribute to one shared branch without ever silently losing acknowledged work
(see `docs/ROADMAP_REVISED.md`; "ARC" was the project's earlier
working name).

**Current state:** pre-release **v0.1.0-alpha.3**
([download](https://github.com/omer-ulucan/concurv/releases/tag/v0.1.0-alpha.3)).

- **One machine:** several private workspaces on shared named branches,
  compare-and-swap publication, three-way merge with line-level merging of
  non-overlapping edits, recorded conflicts that you resolve and save,
  workspace-scoped `undo`, Git-compatible ignores.
- **Across machines:** a self-hosted relay (`concurvd`) holds the published
  branches; `concurv clone | fetch | push` exchange work over pinned TLS
  without ever publishing or overwriting anything implicitly.
- **Protected branches** (R4): a branch can require a passing, recorded
  check of exactly the snapshot about to be published (`concurv check`,
  RFC 0005).
- **Agent coordination** (R5): advisory intents, warnings about
  overlapping or dependent work (a signature changed on another machine
  warns before you publish code using it), and the operation log as JSON
  events (RFC 0006).
- **Developer beta pieces** (R6): tags, Git export and import of tags,
  import into existing history (repeated Git → concurv → Git round trips
  that keep commit ids), configuration files, shell completions,
  contributor and security documents.

Gates passed: R1, R1.5, R2, R3, R4 and R5 (since R3 on Windows and Linux;
macOS last ran at R2.5). R2.5 awaits the owner's final acceptance of the
branch model (ADR 0007); R6 awaits a license and macOS release builds.
Stress-tested with concurrent writers, killed processes (including the
relay) and simulated disconnects at every byte of a transfer; see
`docs/ROADMAP_STATUS.md`. **Not yet:** a license (the owner's decision),
macOS release builds.

> **Alpha:** the on-disk format and the CLI/JSON contract may still change
> before 0.1.0. **No license has been chosen yet**: until one is added, the
> code and binaries are published for evaluation only, with no license granted.

## Install

Download the archive for your platform from the
[latest release](https://github.com/omer-ulucan/concurv/releases) (Linux
x86_64, static; Windows x86_64), check it against `SHA256SUMS`, and put
`concurv` and `concurvd` on your `PATH`. There is no macOS binary for
alpha.3; on a Mac (or anywhere), build from source with Rust 1.89+:

```console
$ cargo install --git https://github.com/omer-ulucan/concurv --tag v0.1.0-alpha.3 concurv-cli
```

Shell completion: `concurv completions bash|zsh|fish|powershell|elvish`
(user guide §11).

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

Branches (ADR 0007):

```console
$ concurv branch create feature
$ concurv workspace add ../my-project-feature --branch feature
$ concurv merge feature                   # (on main) merge the feature, then publish
$ concurv branch switch feature           # or move this workspace to another branch
$ concurv branch create topic --switch    # keep unpublished saves on a new branch
```

Across machines, through a relay you run yourself (RFC 0003, user guide §8a):

```console
$ concurvd init relay-dir && concurvd device add relay-dir laptop   # prints fingerprint, token
$ concurvd serve relay-dir --listen 0.0.0.0:7461
$ concurv clone concurv://relay.example:7461 work --fingerprint sha256:… --token-file laptop.token
$ concurv save -m "work" && concurv publish && concurv push   # exit 42 if the relay has newer work
$ concurv fetch && concurv update                               # bring others' work in
$ concurv merge remote/origin/main                               # when both sides published offline
```

Share with Git / GitHub (needs `git` on PATH):

```console
$ git init --bare ../my-project-mirror.git
$ concurv tag create v1.0                        # a name that never moves (ADR 0008)
$ concurv git export ../my-project-mirror.git --tags   # main's history (and tags) as Git commits
$ concurv git import ../some-git-repo --tags     # start from Git, or bring in new Git commits later
$ concurv git import ../some-git-repo --into git-main  # Git diverged? import beside, then `concurv merge git-main`
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
- Sync never changes working files and never publishes implicitly: a relay
  branch moves only by compare-and-swap, only forward; diverged work is
  refused until you merge it. Everything received is verified and checked
  for completeness before it is stored; a relay whose history went
  backwards is reported, not followed. A retried push never publishes twice.
- Not implemented yet: rename detection, garbage collection, deleting
  branches or tags at the relay (`branch delete` and `tag delete` are
  local), symlinks on Windows, several repositories per relay.
- Don't keep repositories in cloud-synced folders (OneDrive, Dropbox).

## Build and test

```console
$ cargo build --release                    # target/release/concurv
$ cargo test --workspace --all-targets
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo fmt --all -- --check
```

The full gate (the same steps hosted CI ran: fmt, clippy, every test, release
build, MSRV check) runs locally on a clean checkout of a commit:

```console
$ scripts/ci-local.sh [REV]   # Linux, WSL, macOS or Git Bash on Windows
$ scripts/ci-all.sh [REV]     # from Git Bash: Windows and WSL at once
```

GitHub Actions workflows are kept but start only manually for now; gate
evidence since 2026-11-12 comes from these local runs (Windows and Linux;
macOS is not covered locally).

Workspace layout: `crates/concurv-{core,objects,refs,ops,workspace,git,sync,cli,bench}`
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
| `docs/rfcs/0003-sync-protocol.md` | sync between machines: relay, pinned TLS, devices and scopes, wire protocol, fetch/push rules, quarantine (implemented, version 1) |
| `docs/rfcs/0004-workspaces-and-concurrency.md` | workspaces, save vs publish, update and merge, locks and concurrency |
| `docs/rfcs/0005-verified-integration.md` | protected branches: policies, `concurv check`, attestations, relay rules (implemented) |
| `docs/rfcs/0006-agent-coordination.md` | intents, overlap warnings, events; measured accuracy (implemented) |
| `docs/adr/0001-storage-format.md` | why a native BLAKE3 store with one mutable pointer |
| `docs/adr/0002-git-interop.md` | the Git bridge: how export/import work, what is and isn't preserved |
| `docs/adr/0003-project-rename-concurv.md` | the ARC → concurv rename and every identifier it changed |
| `docs/adr/0004-ignore-rules.md` | Git-compatible ignores and the tracked-file model |
| `docs/adr/0005-stat-cache.md` | the stat cache and why it can never produce a wrong snapshot |
| `docs/adr/0006-hunk-merge.md` | line-level merge in `update` |
| `docs/adr/0007-branch-model.md` | branches vs workspaces, naming, transactional switch, `merge`, reference kinds, R3 reconciliation rules |
| `docs/adr/0008-tags.md` | tags: model, names, format 4, Git export and import of tags |
| `docs/security/threat-model.md` | threats and mitigation status |
| `docs/benchmarks/comparison-plan.md` | competitor reproduction plan and the results reproduced so far (concurv and Git; `bench/scenarios/`, `bench/results/`) |
| `bench/RESULTS.md` | `status`/`save` benchmark results (reproducible with `concurv-bench`) |
| `docs/releases/` | release notes |
| `CHANGELOG.md` | changes since the start of the project |
| `CONTRIBUTING.md`, `SECURITY.md` | how to contribute (rules every change follows), how to report a vulnerability privately |
