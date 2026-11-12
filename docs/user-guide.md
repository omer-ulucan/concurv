# concurv user guide

This guide covers what the current build actually does: a single-machine VCS
with several private workspaces sharing named branches (`main` first).
Cross-machine
sync does not exist yet (see `docs/ROADMAP_STATUS.md`).

## 1. Install

**Prebuilt binaries:** download the archive for your platform from
<https://github.com/omer-ulucan/concurv/releases> (Linux x86_64 glibc, macOS
Apple Silicon and Intel, Windows x86_64), verify it with `sha256sum -c
SHA256SUMS --ignore-missing`, unpack it, and put `concurv` (`concurv.exe`) on
your `PATH`. The binaries are not code-signed; on macOS, run
`xattr -d com.apple.quarantine concurv` once if Gatekeeper blocks it.

**From source** (Rust 1.89 or newer):

```console
$ cargo install --git https://github.com/omer-ulucan/concurv --tag v0.1.0-alpha.1 concurv-cli
```

Or from a clone:

```console
$ git clone https://github.com/omer-ulucan/concurv.git
$ cd concurv
$ cargo install --path crates/concurv-cli    # installs `concurv` into ~/.cargo/bin
$ concurv --version
concurv 0.1.0-alpha.1
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

- **Ignoring files works like Git.** Put patterns in `.gitignore` files (any
  directory; deeper files win, `!` re-includes) or in the local, never-shared
  `.concurv/exclude`. `concurv check-ignore <path>` tells you why a path is or
  isn't ignored. `.concurv/` and `.git/` are never versioned, and neither are
  nested repositories. Details: section 3a below.
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

## 3a. Ignoring files

concurv reads the same ignore files Git does:

```gitignore
# .gitignore at the project root
/target
node_modules/
.env
*.log
!keep.log
```

- **Sources**, from strongest to weakest: the `.gitignore` in the deepest
  directory, then shallower ones, then the local `.concurv/exclude` (yours
  only; never saved or shared, like `.git/info/exclude`).
- **Rules** are Git's: `#` comments, `\#` and `\!` escapes, trailing spaces
  ignored, `!` re-include, `/` anchors to the file's directory, a trailing
  `/` matches directories only, `*`, `?`, `[abc]`, `**`. A file inside an
  excluded directory cannot be re-included, exactly as in Git.
- **Already-tracked files stay tracked.** Adding a rule that matches a file
  you've already saved doesn't drop it: changes still show up and are saved,
  and deleting it still shows as `deleted`. Rules only hide **untracked**
  files.
- **Ignored files are never touched.** `restore`, `update`, `undo` and
  workspace checkouts refuse (exit 20, nothing modified) rather than
  overwrite an ignored file, even with `--force`, because a backup can't
  contain it. Move it away first.
- `.concurv/` and `.git/` are never versioned, and a subdirectory that is its
  own Git or concurv repository is skipped.
- Matching is **case-sensitive on every platform** (Git on Windows and macOS
  defaults to case-insensitive).
- Concurv agrees with Git on everything its differential test covers
  (`crates/concurv-workspace/tests/ignore_vs_git.rs`). One known difference:
  for a reversed range like `[z-a]`, Git matches the first character; concurv
  matches nothing.
- Ignore rules **don't protect secrets** that were already saved; they only
  keep new untracked files out.

Ask why a path is or isn't ignored:

```console
$ concurv check-ignore target/debug/app src/main.rs build/
target/debug/app: ignored; parent `target/` excluded by `/target` in .gitignore
src/main.rs: not ignored; no rule matches
build/: not ignored; no rule matches
```

A pattern concurv cannot use (for example an unclosed `[`) never matches, as in
Git, and `check-ignore` lists it under a warning.

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
| both sides have new work | **three-way merge**: changes to *different files* merge, and so do edits to *different parts of the same text file* (separated by at least one unchanged line; listed as "merged line by line"). A merge snapshot is created and checked out. Run your tests before publishing |
| both sides changed the **same lines** of a file (or touching lines, or a binary file, or one deleted what the other changed) | refused with exit 43 `CONCURV_CONFLICT`, listing the paths; **nothing is modified**. Use `update --allow-conflicts` to resolve them (below) |
| a file on one side is a directory on the other | always refused, nothing modified |

`update` needs a clean working copy, so `save` first. It never publishes.

What `publish` does: if `main` is an ancestor of your head, `main` moves to
your head atomically. If someone else published first, it is refused with
exit 42 `CONCURV_DIVERGED` and nothing changes. Then run `update`, test, and
`publish` again. Agents can simply loop `update` → test → `publish` until
publish succeeds.

**A clean merge is not proof the code works.** Two edits to
different files, or to different parts of one file, can still break the build together. That is why the merge
happens in your workspace, where you can test it, and not in `main`.

### Resolving conflicts

```console
$ concurv update --allow-conflicts      # writes the conflicts into your files
$ concurv status                         # lists each conflict: resolved or not
  ... edit files, delete the side files ...
$ concurv save -m "Merge main"           # completes the merge (both parents)
$ concurv publish
```

With `--allow-conflicts`, every change that merges cleanly is applied, and
each conflicted file `P` is written for you to resolve:

- **Text files** changed on both sides contain conflict markers around the
  *whole file*:
  ```
  <<<<<<< ours (workspace b, snapshot 1a2b…)
  your version
  ||||||| base (snapshot 9f8e…)
  the common ancestor
  =======
  their version
  >>>>>>> theirs (main, snapshot 4c5d…)
  ```
  Edit `P` to the right result and remove the marker lines.
- **Binary files, symlinks, and delete/modify conflicts** keep your version at
  `P` (or no file, if you deleted it). Their version is in
  `P.concurv-theirs`. Keep, replace or delete `P` as needed.
- Every conflicted path also gets **`P.concurv-conflict`**, a short note
  explaining what each side did. **Delete it once `P` is resolved**, together
  with any `P.concurv-theirs`.

`save` refuses (exit 43) until every side file is gone and no marker lines
remain, so a half-resolved merge can never be recorded or published. Your
workspace head does not move until that save.

Changed your mind? `concurv update --abort` puts your files back to your head
and ends the merge. If you had already edited files, they are kept in a backup
snapshot first. Right after `update --allow-conflicts` (before editing),
`concurv undo` does the same.

Markers cover the whole file: a file is either merged line by line
completely or left entirely to you. For large files, a diff tool on `P` (ours), `P.concurv-theirs` or the
marker sections helps.

Many workspaces can save, update and publish at the same time: commits are
serialized internally, and nobody's acknowledged save is ever lost. This is
tested with 2, 5 and 10 concurrent writers, and with processes killed mid-command.

## 4a. Branches

A **branch** is a shared, named reference to a snapshot (`main` is the
first one). A **workspace** is your private copy: files, your own head and
any merge in progress. Every workspace **tracks** exactly one branch.
`publish` moves that branch (compare-and-swap: refused if someone else moved
it first), and `update` brings it in. Several workspaces can track the same
branch. Rules and rationale: `docs/adr/0007-branch-model.md`.

```console
$ concurv branch create feature/login         # at this workspace's head; nothing else moves
$ concurv workspace add ../login --branch feature/login
$ cd ../login && concurv save -m "login form" && concurv publish   # moves feature/login only
$ concurv branch list                         # every branch, its head, who tracks it
```

**Two commands for integrating:**

| Command | Brings in | Then |
|---|---|---|
| `concurv update` | the branch this workspace **tracks** (what others published to it) | `publish` |
| `concurv merge <branch>` | **another** branch, e.g. a finished feature into `main` | `publish` (to your own branch) |

Both use the same merge rules (section 4) and need a clean working copy.
Neither publishes anything by itself.

```console
$ cd ~/code/project                           # tracks main
$ concurv merge feature/login                 # conflicts: --allow-conflicts, or merge --abort
$ concurv publish                             # main now contains the feature
```

**Unpublished saves.** If you saved work on `main` but decide it belongs on
its own branch, put a branch there and move to it in one step. No files
change, and `main` stays where it was:

```console
$ concurv branch create experiment --switch
```

**Switching a workspace to another branch:**

```console
$ concurv branch switch feature/login         # checks out that branch's head
$ concurv undo                                # back to the previous branch and files
```

`branch switch` never loses work. Untracked files that don't collide with
the other branch stay where they are. The switch refuses, with nothing
changed, if:
- a tracked file has unsaved changes, or an untracked or ignored file would
  be overwritten or removed (exit 20);
- a merge is in progress (exit 43);
- your saves would end up on no branch (exit 42). Publish them first, or use
  `branch create <name> --switch`.

If a file can't be written halfway through, the working copy is put back as
it was. If the process is killed, `concurv status` reports an interrupted
switch. Every file then holds its old or its new content, so nothing is
lost: run the same `concurv branch switch <name>` again to finish, or
`concurv restore --snapshot <origin> --force` (shown by `status`) to go back.

**Branch names** follow Git's rules, so they export unchanged
(`concurv git export ../mirror.git --from feature/login`). concurv is
stricter in a few places so names work on every OS:
- ASCII only;
- none of `< > " |`;
- no Windows device names such as `con`;
- no leading `-`, and not `HEAD` or `@`;
- the first parts `refs`, `remote` and `remotes` are reserved;
- no two branches that differ only in case, and not both `x` and `x/y`.

Not supported yet: deleting and renaming branches, protected branches,
tags. `undo` cannot reverse `branch create`: branches are shared, and
someone may already be using it.

**Words used in this guide.** Your *workspace state* (files, head, merge in
progress) is private. A *branch* here is a reference shared by all
workspaces of the repository; on one machine it is also the published head.
With multi-machine sync (R3, not built yet), a relay will hold the
authoritative published heads. Fetched heads will appear as read-only
*remote-tracking* references (`remote/<relay>/<branch>`). They never
overwrite your branches, and divergent work is merged by you before it is
published (`docs/rfcs/0003-sync-protocol.md` §5).

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

## 9. Working with Git and GitHub

concurv can write its history into a Git repository, and start from an
existing Git branch. Both require `git` on your `PATH`.

**Export** `main` to Git, for example to push it to GitHub:

```console
$ git init --bare ../project-mirror.git            # once
$ concurv git export ../project-mirror.git          # writes main as Git commits
$ git -C ../project-mirror.git push git@github.com:you/project.git main
```

- Only **published** history (`main` and everything before it, merges
  included) is exported. Unpublished saves in workspaces are not.
- Exporting again only adds new commits. The same snapshot always becomes the
  same Git commit, so repeated exports are safe.
- `--branch B` exports to Git branch `B` instead of `main`.
- If the Git branch has commits that concurv doesn't know (someone committed
  in Git directly), export **refuses** with exit 42 and changes nothing in Git.
  Export to a different branch, or bring those changes into concurv by hand.

**Import** a Git branch to start a new concurv repository:

```console
$ mkdir project && cd project && concurv init
$ concurv git import ../existing-git-repo          # --branch B for another branch
```

The whole history of the branch is imported, `main` is set to its head, and
it is checked out. This only works in a **fresh** repository: nothing
published, nothing saved.

Import is all-or-nothing. It refuses (exit 23, listing everything) when the
branch contains something concurv cannot represent: submodules, paths named
`.concurv`, file names that aren't valid UTF-8, or non-UTF-8 commit messages.

What survives a round trip: file contents, executable bits, symlinks,
directory structure, merges, authors and messages. Not preserved: separate
committer identities, timezones and sub-second times. Native Git commit ids
change after passing through concurv (concurv adds a `Concurv-Change-Id`
trailer). Tags, notes and signatures are not imported. Full details:
`docs/adr/0002-git-interop.md`.

## 10. Troubleshooting and recovery

| Symptom | What it means | What to do |
|---|---|---|
| `error[CONCURV_DIRTY_WORKSPACE]` (exit 20) on `restore` or `update` | You have unsaved changes | `concurv save -m …` first. For `restore` only: `--force` keeps a backup snapshot and overwrites |
| `status` shows **WARNING: a restore … was interrupted** | A restore, `update` or `workspace add` checkout crashed or was killed midway; some files are from the old state and some from the new | Re-run the printed command: `concurv restore --snapshot <target> --force`. If a backup id is shown, your pre-restore work is safe in that snapshot |
| `error[CONCURV_LOCKED]` (exit 40) | Another concurv process held the repository or workspace lock for the whole 30 s wait | Retry. Commands normally wait for each other automatically. The lock is held by the OS and released automatically when that process exits, even if it crashed, so there's nothing to delete |
| `error[CONCURV_DIVERGED]` (exit 42) on `publish` | Someone published to `main` after your last `update` | `concurv update`, test, `concurv publish` again. Nothing was changed |
| `error[CONCURV_CONFLICT]` (exit 43) on `update` | Both sides changed the same lines of a file, or a binary file (paths are listed), a file is a directory on the other side, the histories have no single merge base, or a merge is already pending | Nothing was modified. Same-file conflicts: `concurv update --allow-conflicts` and resolve (section 4). A pending merge: finish it with `save` or `update --abort` |
| `error[CONCURV_CONFLICT]` (exit 43) on `save` | A merge is pending and some conflicts are unresolved (listed: markers remain or side files still exist) | Finish resolving, delete the `*.concurv-conflict` / `*.concurv-theirs` files, save again. Or `concurv update --abort` |
| `error[CONCURV_INVALID_ARGUMENT]` (exit 2) on `undo` | Your most recent operation was a `publish` (or `workspace add`), which undo never reverts | Make a new change that reverts the published work, save and publish it |
| `error[CONCURV_NOT_FOUND]` (exit 50) on `undo` | Nothing left to undo in this workspace | — |
| `error[CONCURV_EXTERNAL_TOOL]` (exit 60) on `git export` / `git import` | `git` is not installed, or a git command failed (its message is shown) | Install Git and make sure `git --version` works in the same terminal |
| `error[CONCURV_DIVERGED]` (exit 42) on `git export` | The Git branch has commits that aren't in concurv's `main` | Nothing changed in Git. Export to another branch (`--branch`), or bring those changes into concurv by hand |
| `error[CONCURV_UNSUPPORTED_FILE_TYPE]` (exit 23) on `git import` | The Git history contains something concurv cannot represent (listed) | Nothing was imported. Remove submodules / rename the paths in Git first |
| `error[CONCURV_CONCURRENT_MODIFICATION]` (exit 21) | A file changed while concurv was reading or replacing it (e.g. an editor or build was writing) | Let the other program finish, then retry. Nothing was recorded or overwritten from the inconsistent read |
| `error[CONCURV_UNSUPPORTED_FILE_TYPE]` (exit 23) | A FIFO, socket or device file, or (on Windows) a symlink or junction, is in the project | Move it out of the project directory. R1 cannot version it, and refuses rather than silently skipping it |
| `error[CONCURV_INVALID_PATH]` (exit 22) on `save` | A file name is not valid UTF-8, or contains characters concurv forbids | Rename the file |
| `status` seems to miss a change on a network or FUSE filesystem | That filesystem may not update the inode change time, which the stat cache relies on (Unix only, ADR 0005) | `concurv status --no-cache` hashes every file. `save` never trusts the cache, so snapshots are always correct |
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

## 11. Scripting and agents

Add `--json` to any command for one machine-readable JSON document on stdout,
with `concurv_json_version`, `ok`, `data` and, on failure, `error.code`. Branch
on the exit code or `error.code`, never on human text. The schema and the
full error/exit-code table are in `docs/cli.md`. `-C <dir>` runs a command as
if started in `<dir>`.

## 12. Platform notes

| | Linux | macOS | Windows |
|---|---|---|---|
| Tested | every change, local gate in WSL (x86_64) | hosted CI until 2026-11-02 (Apple Silicon, APFS); not covered by the local gate | every change, local gate (NTFS) |
| Executable bit | recorded | recorded | not available; a file keeps the kind recorded in the head |
| Symlinks | versioned as links, never followed | same | rejected (symlinks and junctions) |
| Directory fsync after rename | yes | yes | not available; relies on NTFS journaling |
| Non-UTF-8 file names | rejected by `save` | cannot exist (APFS refuses them) | rejected (unpaired surrogates) |
| Unsafe-name checks on restore | none needed | case collisions | reserved names, `<>:"\|?*`, trailing dot/space, case collisions, `CONCUR~n` |
| `status`/`diff` stat cache (ADR 0005) | yes | yes | no (always hashes every file) |
