# concurv user guide (R1)

This guide covers what the current build actually does. The R1 kernel is a
single-workspace, single-machine VCS. Sync, merge, undo and multiple
workspaces do not exist yet (see `docs/ROADMAP_STATUS.md`).

## 1. Install

Requires Rust 1.89 or newer.

```console
$ git clone https://github.com/omer-ulucan/concurv.git
$ cd concurv
$ cargo install --path crates/concurv-cli    # installs `concurv` into ~/.cargo/bin
$ concurv --version
concurv 0.1.0
```

Or build without installing: `cargo build --release` puts the binary at
`target/release/concurv` (`concurv.exe` on Windows).

**Where not to put repositories:** do not create repositories inside
cloud-synced folders (OneDrive, Dropbox, iCloud Drive) or on network
filesystems. Their rename and flush behaviour is outside the durability
guarantee.

## 2. Concepts in one minute

- **Working copy:** the files in your project directory. concurv never records
  them until you run `concurv save`.
- **Snapshot:** an immutable, content-addressed record of every file at one
  moment, plus its parent, author, time and message. It's identified by a
  64-character hex id; any unique prefix of at least 8 characters works as input.
- **Head:** the snapshot your working copy is based on. `save` advances it and
  the branch `main`.
- **Operation log:** every `init`, `save` and `restore` is recorded separately
  from snapshot history, including backups made by `restore --force`.

All metadata lives in `.concurv/` at the project root. Never edit it by hand.

## 3. Everyday workflow

```console
$ concurv init                       # once, in the project root
$ concurv status                     # what changed since the head?
$ concurv diff                       # line-level diff of text files
$ concurv save -m "Add parser"       # record a snapshot
$ concurv log                        # history, newest first (-n 10 to limit)
$ concurv op log                     # every init/save/restore, including backups
```

Details that matter:

- **Everything is versioned** except `.concurv/` itself, at any depth. There is
  **no ignore file yet**, so build outputs such as `target/` or `node_modules/`
  are saved too. Keep them outside the project, or accept the extra storage.
- Empty directories are not versioned, the same as Git.
- A rename shows up as `deleted` plus `added`.
- `status` compares file **contents**, not timestamps, so it never misses an
  edit. The cost is that it reads every file on each run.
- `save` with no changes prints `Nothing to save` and records nothing.
- Author: set `CONCURV_AUTHOR="Name <email>"`. Otherwise concurv uses the OS
  user name, falling back to `unknown`.
- `diff` shows text diffs for UTF-8 files up to 8 MiB. Binary, larger and
  symlink changes get a one-line note instead.

## 4. Going back to an earlier snapshot

```console
$ concurv log                                    # find the snapshot id
$ concurv restore --snapshot 1a2b3c4d
```

What `restore` does:

1. It loads and hash-checks the entire target snapshot **before** touching any file.
2. If you have unsaved changes (including new files), it **refuses** with exit
   code 20 and changes nothing.
3. It makes your files match the snapshot: it writes changed files, deletes
   files the snapshot doesn't have, and removes directories left empty.
4. **History does not move.** Your head is still the latest snapshot, so
   `status` now shows the difference. Run `concurv save -m "Revert to …"` to
   record the restored state as a new snapshot on top.

### Overwriting unsaved changes safely: `--force`

```console
$ concurv restore --snapshot 1a2b3c4d --force
Restored working copy to 1a2b3c4d… (2 written, 1 deleted). History unchanged; `concurv save` to record it.
Previous working copy saved as backup snapshot 9f8e7d6c… (recover with `concurv restore --snapshot 9f8e7d6c… --force`).
```

With `--force`, concurv first saves your current working copy, unsaved edits
included, as a **backup snapshot**, and only then overwrites files. To get your
work back, restore the backup id.

Backups don't appear in `concurv log`, because they aren't part of the branch
history. If you lost the printed id, find it in the operation log:

```console
$ concurv op log -n 5
operation 7c1e… (restore)
Date:   2026-11-01 14:02:11 UTC
    restore working copy to 1a2b3c4d…
    head after: 5d6e…
    backup of discarded working copy: 9f8e7d6c…
    (recover with `concurv restore --snapshot 9f8e7d6c… --force`)
```

## 5. The operation log

`concurv op log` lists every repository operation, newest first: `init`,
`save` and `restore`. Each entry shows its time and description, the head
snapshot after it ran and, for `restore --force`, the backup snapshot of the
work it replaced. `-n N` limits the output, and `--json` gives the full
record, including parent operations and branch heads. The command is
read-only.

The operation log is separate from snapshot history (`concurv log`): a
`restore` adds an operation but no snapshot. Every backup stays recorded there
permanently. Nothing is ever garbage-collected in R1.

## 6. Checking integrity

```console
$ concurv verify          # everything reachable from history
$ concurv verify --full   # additionally every object file, including unreferenced ones
```

`verify` never modifies anything. Exit 0 means intact, 30 means a corrupt object,
and 31 means a missing object. Each problem lists the object id and reason.

## 7. Troubleshooting and recovery

| Symptom | What it means | What to do |
|---|---|---|
| `error[CONCURV_DIRTY_WORKSPACE]` (exit 20) on `restore` | You have unsaved changes | `concurv save -m …` first, or use `--force` to keep a backup and overwrite |
| `status` shows **WARNING: a restore … was interrupted** | A restore crashed or was killed midway; some files are from the old state and some from the new | Re-run the printed command: `concurv restore --snapshot <target> --force`. If a backup id is shown, your pre-restore work is safe in that snapshot |
| `error[CONCURV_LOCKED]` (exit 40) | Another concurv process is saving or restoring | Wait and retry. The lock is held by the OS and released automatically when that process exits, even if it crashed, so there's nothing to delete |
| `error[CONCURV_CONCURRENT_MODIFICATION]` (exit 21) | A file changed while concurv was reading or replacing it (e.g. an editor or build was writing) | Let the other program finish, then retry. Nothing was recorded or overwritten from the inconsistent read |
| `error[CONCURV_UNSUPPORTED_FILE_TYPE]` (exit 23) | A FIFO, socket or device file, or (on Windows) a symlink or junction, is in the project | Move it out of the project directory. R1 cannot version it, and refuses rather than silently skipping it |
| `error[CONCURV_INVALID_PATH]` (exit 22) on `save` | A file name is not valid UTF-8, or contains characters concurv forbids | Rename the file |
| `error[CONCURV_INVALID_PATH]` on `restore` (Windows/macOS) | The snapshot has names this OS can't create safely (`CON`, `a:b`, names differing only in case, …) | Restore on Linux, or rename those files there and save. Nothing was modified |
| `verify` reports `CONCURV_OBJECT_CORRUPT` / `CONCURV_OBJECT_MISSING` | Storage damage (disk fault, manual edits, an aborted copy) | See below |
| `error[CONCURV_UNSUPPORTED_FORMAT]` (exit 12) | The repository was written by an incompatible concurv version | Use the matching concurv version. concurv never guesses at unknown formats |
| `error[CONCURV_NOT_A_REPO]` (exit 10) | No `.concurv/` in this directory or any parent | `concurv init`, or `cd` into the project. Repositories made by pre-rename `arc` builds (with an `.arc/` folder) aren't recognised (ADR 0003) |

### Corruption

1. **Stop and copy the whole project directory**, including `.concurv/`,
   somewhere safe before trying anything else.
2. Run `concurv verify --full` and note the affected ids.
3. If the damaged object is file content that still exists unchanged in your
   working copy, the next `concurv save` rewrites it. Any corrupt copy is moved
   to `.concurv/quarantine/` and kept for diagnosis, never deleted. Re-run
   `verify` to confirm.
4. concurv never "repairs" by guessing. Restoring a snapshot whose objects are
   damaged fails before any file is touched.

### Backing up a repository

Copy the entire project directory while no `save` or `restore` is running.
Objects are immutable and the head is a single small file, so a copy taken
between commands is consistent.

## 8. Scripting and agents

Add `--json` to any command for one machine-readable JSON document on stdout,
with `concurv_json_version`, `ok`, `data` and, on failure, `error.code`. Branch
on the exit code or `error.code`, never on human text. The schema and the
full error/exit-code table are in `docs/cli.md`. `-C <dir>` runs a command as
if started in `<dir>`.

## 9. Platform notes

| | Linux | macOS | Windows |
|---|---|---|---|
| Tested on this build | not yet | not yet | yes (Windows 11, NTFS) |
| Executable bit | recorded | recorded | not available; a file keeps the kind recorded in the head |
| Symlinks | versioned as links, never followed | same | rejected (symlinks and junctions) |
| Directory fsync after rename | yes | yes | not available; relies on NTFS journaling |
| Unsafe-name checks on restore | none needed | case collisions | reserved names, `<>:"\|?*`, trailing dot/space, case collisions, `CONCUR~n` |
