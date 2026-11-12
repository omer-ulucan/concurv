# Contributing to concurv

Thanks for your interest. concurv is an **alpha**: the on-disk format and the
CLI/JSON contract can still change before 0.1.0, and the project is run by
one maintainer.

> **No license has been chosen yet** (see the README). Until there is one,
> please start with an issue rather than a pull request: the terms for
> accepting contributions will be stated here together with the license.
> Bug reports, reproductions and design discussion are welcome now.

## Reporting a bug

Open an issue with:

- `concurv --version`, the operating system and the filesystem (NTFS, ext4,
  APFS, a network or cloud-synced folder…);
- the commands you ran and their output, ideally with `--json`;
- whether `concurv verify --full` passes, and the last entries of
  `concurv op log`;
- for sync problems, the relay's version and how the relay was started.

**Never paste device tokens** (`.concurv/remotes/<name>/token`, the output of
`concurvd device add`). Security problems go through `SECURITY.md`, not
public issues.

## Building and testing

Rust 1.89 or newer (the MSRV). From the repository root:

```console
$ cargo fmt --all -- --check
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo test --workspace --all-targets
$ cargo build --release
```

The full gate runs these steps, plus the MSRV check, on a fresh clone of a
commit:

```console
$ scripts/ci-local.sh [REV]   # Linux, WSL, macOS or Git Bash on Windows
$ scripts/ci-all.sh [REV]     # from Git Bash: Windows and WSL at once
```

On Windows, Clippy never compiles `#[cfg(unix)]` code. Also run
`cargo clippy --workspace --all-targets --target x86_64-unknown-linux-gnu -- -D warnings`
(after `rustup target add x86_64-unknown-linux-gnu`), or the gate in WSL.

## Rules every change follows

These protect the project's central promise: acknowledged work is never lost.

1. **Never lose acknowledged data.** `.concurv/op_head` is the only mutable
   pointer and may only ever name durable, verified objects. Objects are
   immutable; nothing rewrites or deletes them.
2. **Never overwrite a user's files silently.** `restore` refuses a dirty
   working copy without `--force`, and `--force` saves a backup snapshot
   first. Ignored and untracked files are never overwritten.
3. **The byte format is normative** (`docs/rfcs/0001-vcs-semantics.md`).
   Changing it needs a new repository format version, an upgrade path, and
   test vectors. Identifier names are part of the format (ADR 0003).
4. **The CLI contract is stable** (`docs/cli.md`): commands, JSON fields,
   error codes and exit codes. A new command is documented there in the
   same change; a command is never documented before it exists.
5. **No `unsafe`, no `unwrap`/`expect` on runtime paths, no ignored tests.**
   Every behaviour has a test; failure paths (crashes at each write step,
   transient errors, killed processes, hostile input) have tests too.
6. **Claims need evidence.** A roadmap stage is marked passed only with the
   gate run that proves it (`docs/ROADMAP_STATUS.md`). Documentation states
   what is *not* implemented or not proven, as plainly as what is.

Before changing storage, references, restore, sync or the CLI, read the
relevant RFC (`docs/rfcs/`), ADR (`docs/adr/`) and
`docs/security/threat-model.md`. Larger changes start with a short design
note (a new RFC or ADR) so the trade-offs are written down before the code.

## Workflow

- Work on a branch (`feat/…`, `fix/…`, `docs/…`); `main` only receives
  changes whose gate has passed.
- Small commits with a type prefix, as in the history: `feat(sync): …`,
  `fix(cli): …`, `test(r4): …`, `docs: …`, `ci: …`.
- Update `CHANGELOG.md` (under *Unreleased*) and the user guide when
  behaviour changes.

## Where things are

| Path | Contents |
|---|---|
| `crates/concurv-core` | ids, paths, errors and exit codes |
| `crates/concurv-objects` | object encoding and the content-addressed store |
| `crates/concurv-workspace` | repository, workspaces, save/restore/merge, branches, checks, coordination |
| `crates/concurv-git` | Git export/import through `git fast-import`/`fast-export` |
| `crates/concurv-sync` | relay, wire protocol, pinned TLS, fetch/push/clone |
| `crates/concurv-cli` | the `concurv` and `concurvd` binaries |
| `docs/` | user guide, CLI contract, RFCs, ADRs, threat model, roadmap and evidence |
