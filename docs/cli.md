# `concurv` CLI contract

Only the commands below exist. Anything else is a usage error (exit 2).
Every command runs in the workspace containing the current directory (or `-C`).

| Command | Effect | Mutates |
|---|---|---|
| `concurv init` | create `.concurv` in the current directory (atomic); it becomes workspace `default` | creates repo |
| `concurv status` | changes of working copy vs this workspace's head; warns about interrupted restores | no |
| `concurv diff` | unified text diffs; binary/large/symlink changes are summarized | no |
| `concurv save -m <msg>` | durably snapshot the working copy and advance **this workspace's head**. Does not publish | workspace history |
| `concurv publish` | CAS fast-forward of the workspace's branch (`main`) to the workspace head | shared branch |
| `concurv update [--allow-conflicts \| --abort]` | bring `main` into this workspace: fast-forward, or file-level merge into a new snapshot. `--allow-conflicts`: write conflicts into the working copy and start a pending merge; `--abort`: give a pending merge up | working copy, workspace head |
| `concurv workspace add <path> [--name N]` | create a workspace in a new or empty directory and check out `main` there | creates workspace |
| `concurv workspace list` | all workspaces with head and path | no |
| `concurv undo` | reverse this workspace's most recent operation (`save`, `update`, `restore`); repeat to go further back. Refuses `publish` | workspace head and/or working copy |
| `concurv log [-n N]` | first-parent snapshot history of this workspace's head | no |
| `concurv restore --snapshot <id> [--force]` | make working files equal to the snapshot; history unchanged | working copy (+ backup snapshot, operation) |
| `concurv check-ignore <path>…` | explain whether each path is ignored (`.gitignore`, `.concurv/exclude`) or tracked, and by which rule; reports unusable patterns | no |
| `concurv verify [--full]` | integrity check of reachable graph (`--full`: every object file) | no |
| `concurv op log [-n N]` | operation log of all workspaces, newest first, with backups | no |
| `concurv git export <repo> [--branch B]` | write `main`'s history into a Git repository and move Git branch `B` (default `main`) by compare-and-swap. Requires `git` on PATH | the Git repository |
| `concurv git import <repo> [--branch B]` | import Git branch `B` into this **fresh** repository, publish it as `main` and check it out | history, working copy |

Global flags: `--json`, `-C <dir>`. Author comes from `CONCURV_AUTHOR`, else the OS
user name (`USER`/`USERNAME`), else `unknown`.

**Behaviour change in R2:** `save` no longer moves `main`; `publish` does (RFC 0004 §2).

`restore` refuses a dirty working copy (exit 20). With `--force` it first
saves the working copy as a backup snapshot (printed, and recorded in the
operation log, listed by `concurv op log`); recover with
`concurv restore --snapshot <backup> --force`. `update` also refuses a dirty
working copy (exit 20) and has no `--force`.

Mutating commands wait up to 30 s for a busy repository or workspace lock,
then fail with exit 40.

## JSON envelope (`concurv_json_version` 1)

```json
{ "concurv_json_version": 1, "command": "status", "ok": true, "data": { ... } }
{ "concurv_json_version": 1, "command": "status", "ok": false,
  "error": { "code": "CONCURV_NOT_A_REPO", "message": "...", "exit_code": 10 } }
```

`verify` with problems returns `ok: false` **and** `data` (the report).
Additive fields may appear in later versions; removing or changing the meaning
of a field requires bumping `concurv_json_version`. (The R2 change to `save`
changes behaviour, not the JSON shape.)

`data` per command (`command` is the space-separated command name, e.g. `"workspace add"`):

- `init`: `root`, `branch`
- `status`: `branch`, `head` (hex or null), `clean`, `changes[]`, `merge` (null, or `{conflict, theirs, conflicts[]: {path, kind, resolved, markers_remaining, remaining_side_files[]}}`)
  (`path`, `kind` ∈ `added|deleted|modified|type_changed`, `old_blob`, `new_blob`),
  `interrupted_restore` (null or `{target, backup}`)
- `diff`: `files[]` (`path`, `kind`, `unified` or null, `note` or null)
- `save`: `created`, `snapshot`, `operation`, `changes[]`
- `publish`: `branch`, `from` (previous branch head or null), `to`, `published` (false if already there), `operation`
- `update`: `branch`, `result` ∈ `up_to_date|fast_forward|merged|conflicted|aborted`, `conflict` (pending-merge id or null), `conflicts[]` (`path`, `kind` ∈ `both_modified|both_added|delete_modify|file_directory`, `markers`, `side_files[]`), for `aborted` also `backup`, `head`, `written`, `deleted`, `operation`
- `git export`: `branch`, `tip` (Git commit), `previous` (Git branch before, or null), `exported` (new commits), `reused` (already present), `updated`
- `git import`: `branch`, `git_tip`, `tip` (snapshot now at `main`), `commits`, `written`
- `undo`: `undone` (operation id), `undone_kind` ∈ `save|update|restore`, `undone_description`, `head` (workspace head after), `files_restored_to` (snapshot or null if files were untouched), `written`, `deleted`, `operation`
- `workspace add`: `workspace` (as below), `written`
- `workspace list`: `workspaces[]` (`id`, `name`, `branch`, `head`, `root` (null if unknown on this device), `primary`, `current`)
- `log`: `snapshots[]` (`snapshot`, `parents[]`, `tree`, `change_id`, `author`, `timestamp_ms`, `message`)
- `restore`: `target`, `backup`, `written`, `deleted`, `operation`
- `check-ignore`: `paths[]` (`input`, `path`, `is_dir`, `tracked`, `ignored`, `rule` (null or `{source, pattern, negated}`), `excluded_parent` (null or a directory)), `unusable_patterns[]` (`source`, `line`, `pattern`, `reason`)
- `verify`: `full`, `operations`, `conflicts`, `snapshots`, `trees`, `blobs`, `unreachable`, `problems[]` (`code`, `subject`, `message`), `notes[]`
- `op log`: `operations[]` (`operation`, `parents[]`, `kind` ∈ `init|save|restore|workspace-add|publish|update|undo|merge-abort|git-import`, `description`, `timestamp_ms`, `device`, `workspace` (null for `init`), `backup` (null unless `restore --force` replaced unsaved work), `undoes` (for `undo`: the reversed operation), `checked_out` (snapshot the working copy was set to, or null), `head` (the **current** workspace's head after the operation, or null), `branches` (object: name → snapshot id))

## Error codes and exit codes

| Exit | Code | Meaning |
|---|---|---|
| 0 | — | success |
| 1 | `CONCURV_IO`, `CONCURV_INTERNAL` | OS error / bug |
| 2 | `CONCURV_INVALID_ARGUMENT` (and clap usage errors) | bad input (e.g. empty message, bad workspace name, publish without a saved snapshot, undo of a `publish`) |
| 10 | `CONCURV_NOT_A_REPO` | no repository found, or a workspace whose repository is gone |
| 11 | `CONCURV_ALREADY_INITIALIZED` | `init` on existing repo |
| 12 | `CONCURV_UNSUPPORTED_FORMAT` | unknown object/repository format version |
| 20 | `CONCURV_DIRTY_WORKSPACE` | restore/update/undo would discard unsaved changes, or would overwrite an ignored untracked file (also with `--force`) |
| 21 | `CONCURV_CONCURRENT_MODIFICATION` | a file changed while concurv was reading/replacing it |
| 22 | `CONCURV_INVALID_PATH` | name violates concurv or platform path rules (incl. a `.git` entry on restore); nested workspace |
| 23 | `CONCURV_UNSUPPORTED_FILE_TYPE` | FIFO/socket/device, or link on Windows; `git import` of something concurv cannot represent (submodule, `.concurv` path, non-UTF-8 message) |
| 30 | `CONCURV_OBJECT_CORRUPT`, `CONCURV_WRONG_OBJECT_KIND` | integrity failure |
| 31 | `CONCURV_OBJECT_MISSING` | referenced object absent |
| 33 | `CONCURV_LIMIT_EXCEEDED` | input exceeds a hard limit |
| 40 | `CONCURV_LOCKED` | a repository or workspace lock stayed busy for the whole wait |
| 41 | `CONCURV_STALE_HEAD` | op head or workspace head moved during compare-and-swap |
| 42 | `CONCURV_DIVERGED` | `publish` refused: the branch has work this workspace has not integrated (run `update`); `git export` refused: the Git branch has commits outside `main`'s history |
| 43 | `CONCURV_CONFLICT` | `update` refused: conflicting changes (without `--allow-conflicts`), file/directory conflict, ambiguous merge base, or a merge already pending; `save` refused: unresolved conflicts. Nothing modified |
| 50 | `CONCURV_NOT_FOUND` | id/prefix not found or not a snapshot; workspace not registered; nothing to undo |
| 51 | `CONCURV_AMBIGUOUS_ID` | prefix matches several objects |
| 60 | `CONCURV_EXTERNAL_TOOL` | `git` is not installed or a git command failed |
