# concurv user guide

This guide covers what the current build actually does: a single-machine VCS
with several private workspaces sharing one branch (`main`). Cross-machine
sync and same-file conflict resolution do not exist yet (see
`docs/ROADMAP_STATUS.md`).

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
- **Workspace:** a directory with its own working copy and its own head. The
  directory where you ran `init` is workspace `default`; `workspace add`
  creates more. A command only ever changes the files of the workspace it runs in.
- **Head:** the snapshot this workspace's working copy is based on. `save`
  advances it. It is private to the workspace.
- **`main`:** the shared branch. It moves only when you run `publish`, and only
  forward.
- **Operation log:** every operation of every workspace is recorded separately
  from snapshot history, including backups made by `restore --force`.

All metadata lives in `.concurv/` at the project root. Never edit it by hand.

## 3. Everyday workflow

```console
$ concurv init                       # once, in the project root
$ concurv status                     # what changed since the head?
$ concurv diff                       # line-level diff of text files
$ concurv save -m "Add parser"       # record a snapshot in this workspace
$ concurv publish                    # make it the new `main`
$ concurv log                        # history, newest first (-n 10 to limit)
$ concurv op log                     # every operation, including backups
$ concurv undo                       # reverse your last save/update/restore
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
- `save` records your work durably **in your workspace only**. Nobody else
  sees it until you `publish`.

## 4. Working in parallel: workspaces, update and publish

Give every person, agent or task its own workspace. Never let two of them share
one directory.

```console
$ cd ~/code/project                      # the workspace created by `init`
$ concurv workspace add ../project-agent-a
$ concurv workspace add ../project-agent-b --name reviewer
$ concurv workspace list
```

A new workspace must be a new or empty directory, and must not be inside
another workspace (or contain one). It starts with a checkout of `main`.

The cycle in each workspace:

```console
$ concurv save -m "Implement token refresh"   # private, durable
$ concurv update                              # bring in what others published
$ cargo test                                  # test the combined result here
$ concurv publish                             # main := your head (compare-and-swap)
```

What `update` does:

| Situation | Result |
|---|---|
| `main` has nothing new | `up_to_date`, nothing changes |
| you have no unpublished work | fast-forward: your files and head move to `main` |
| both sides have new work | **file-level merge**: if the two sides changed *different files*, a merge snapshot is created and checked out. Run your tests before publishing |
| both sides changed the **same file** (or deleted/changed, or a file vs a directory) | refused with exit 43 `CONCURV_CONFLICT`, listing the paths; **nothing is modified** |

`update` needs a clean working copy, so `save` first. It never publishes.

What `publish` does: if `main` is an ancestor of your head, `main` moves to
your head atomically. If someone else published first, it is refused with
exit 42 `CONCURV_DIVERGED` and nothing changes. Then run `update`, test, and
`publish` again. Agents can simply loop `update` → test → `publish` until
publish succeeds.

**A clean file-level merge is not proof the code works.** Two edits to
different files can still break the build together. That is why the merge
happens in your workspace, where you can test it, and not in `main`.

**Same-file conflicts** cannot be resolved inside concurv yet (planned). The
workaround: create a fresh workspace from `main` (`concurv workspace add`),
re-apply your change to the conflicting files there, save and publish. Your
old workspace's snapshots stay in history.

Many workspaces can save, update and publish at the same time: commits are
serialized internally, and nobody's acknowledged save is ever lost. This is
tested with 2, 5 and 10 concurrent writers, and with processes killed mid-command.

## 5. Going back to an earlier snapshot

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

Simpler: right after a forced restore, `concurv undo` puts the unsaved work
back exactly (next section).

## 6. Undo

`concurv undo` reverses the most recent operation **of the workspace you are
in**, and exactly what that operation changed:

| You last ran | `undo` does |
|---|---|
| `save` | moves your head back to before the save. Your files are **not** touched, so the saved changes show up in `status` as unsaved again |
| `update` | puts your files and head back to before the update (the merge or fast-forward disappears from your workspace) |
| `restore` | puts your files back to what they were before the restore. After `restore --force`, that means your unsaved work comes back |
| `publish` | **refused** (exit 2). Others may already build on what you published; make a new change that reverts it instead |

Run `undo` again to go further back: it skips operations that were already
undone. Undo itself cannot be undone; to return to a state, find its snapshot
in `concurv op log` and `restore` it.

Undo is safe:

- It only ever changes **your** workspace. Other workspaces' heads, their files
  and `main` are never touched, even if they ran commands in between.
- If undoing would overwrite files you changed after the operation (for
  `update` and `restore`), it refuses with exit 20 and changes nothing. Save
  or revert your edits first.
- Nothing is deleted: undone snapshots stay in the operation log.

The first checkout of a new workspace (from `workspace add`) cannot be undone.

## 7. The operation log

`concurv op log` lists every repository operation, newest first: `init`,
`save` and `restore`. Each entry shows its time and description, the head
snapshot after it ran and, for `restore --force`, the backup snapshot of the
work it replaced. `-n N` limits the output, and `--json` gives the full
record, including parent operations and branch heads. The command is
read-only.

The operation log is separate from snapshot history (`concurv log`): a
`restore` adds an operation but no snapshot. Every backup stays recorded there
permanently. Nothing is ever garbage-collected in R1.

## 8. Checking integrity

```console
$ concurv verify          # everything reachable from history
$ concurv verify --full   # additionally every object file, including unreferenced ones
```

`verify` never modifies anything. Exit 0 means intact, 30 means a corrupt object,
and 31 means a missing object. Each problem lists the object id and reason.

## 9. Troubleshooting and recovery

| Symptom | What it means | What to do |
|---|---|---|
| `error[CONCURV_DIRTY_WORKSPACE]` (exit 20) on `restore` or `update` | You have unsaved changes | `concurv save -m …` first. For `restore` only: `--force` keeps a backup snapshot and overwrites |
| `status` shows **WARNING: a restore … was interrupted** | A restore, `update` or `workspace add` checkout crashed or was killed midway; some files are from the old state and some from the new | Re-run the printed command: `concurv restore --snapshot <target> --force`. If a backup id is shown, your pre-restore work is safe in that snapshot |
| `error[CONCURV_LOCKED]` (exit 40) | Another concurv process held the repository or workspace lock for the whole 30 s wait | Retry. Commands normally wait for each other automatically. The lock is held by the OS and released automatically when that process exits, even if it crashed, so there's nothing to delete |
| `error[CONCURV_DIVERGED]` (exit 42) on `publish` | Someone published to `main` after your last `update` | `concurv update`, test, `concurv publish` again. Nothing was changed |
| `error[CONCURV_CONFLICT]` (exit 43) on `update` | Both sides changed the same file (paths are listed), or the histories have no single merge base | Nothing was modified. Workaround until conflict resolution lands: new workspace from `main`, re-apply your change there, save, publish (section 4) |
| `error[CONCURV_INVALID_ARGUMENT]` (exit 2) on `undo` | Your most recent operation was a `publish` (or `workspace add`), which undo never reverts | Make a new change that reverts the published work, save and publish it |
| `error[CONCURV_NOT_FOUND]` (exit 50) on `undo` | Nothing left to undo in this workspace | — |
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

## 10. Scripting and agents

Add `--json` to any command for one machine-readable JSON document on stdout,
with `concurv_json_version`, `ok`, `data` and, on failure, `error.code`. Branch
on the exit code or `error.code`, never on human text. The schema and the
full error/exit-code table are in `docs/cli.md`. `-C <dir>` runs a command as
if started in `<dir>`.

## 11. Platform notes

| | Linux | macOS | Windows |
|---|---|---|---|
| Tested (CI on every push) | yes (x86_64) | yes (Apple Silicon, APFS) | yes (NTFS) |
| Executable bit | recorded | recorded | not available; a file keeps the kind recorded in the head |
| Symlinks | versioned as links, never followed | same | rejected (symlinks and junctions) |
| Directory fsync after rename | yes | yes | not available; relies on NTFS journaling |
| Non-UTF-8 file names | rejected by `save` | cannot exist (APFS refuses them) | rejected (unpaired surrogates) |
| Unsafe-name checks on restore | none needed | case collisions | reserved names, `<>:"\|?*`, trailing dot/space, case collisions, `CONCUR~n` |
