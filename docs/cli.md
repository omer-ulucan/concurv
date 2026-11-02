# `concurv` CLI contract

Only the commands below exist. Anything else is a usage error (exit 2).
Every command runs in the workspace containing the current directory (or `-C`).

| Command | Effect | Mutates |
|---|---|---|
| `concurv init` | create `.concurv` in the current directory (atomic); it becomes workspace `default` | creates repo |
| `concurv status [--no-cache]` | changes of working copy vs this workspace's head; warns about interrupted restores. Uses the stat cache on Unix (ADR 0005); `--no-cache` hashes every file | no |
| `concurv diff [--no-cache]` | unified text diffs (Git style, incl. `\ No newline at end of file`; CRLF kept); binary/large/symlink changes are summarized | no |
| `concurv save -m <msg>` | durably snapshot the working copy and advance **this workspace's head**. Does not publish | workspace history |
| `concurv publish` | CAS fast-forward of the branch this workspace tracks (default `main`) to the workspace head | shared branch |
| `concurv update [--allow-conflicts \| --abort]` | bring the **tracked** branch into this workspace: fast-forward, or a three-way merge into a new snapshot (file level, plus line-level merge of text files whose edits do not overlap, ADR 0006). `--allow-conflicts`: write conflicts into the working copy and start a pending merge; `--abort`: give a pending merge up | working copy, workspace head |
| `concurv merge <branch> [--allow-conflicts] \| --abort` | integrate **another** branch with the rules of `update`; the workspace keeps its branch, and only a later `publish` (CAS) publishes the result. Merging the tracked branch is refused (use `update`). `--abort` = `update --abort` (ADR 0007) | working copy, workspace head |
| `concurv workspace add <path> [--name N] [--branch B]` | create a workspace in a new or empty directory, tracking `B` (default: the current workspace's branch), and check out that branch there | creates workspace |
| `concurv workspace list` | all workspaces with branch, head and path | no |
| `concurv branch create <name> [--at <snapshot\|branch> \| --switch]` | add a branch, by default at this workspace's head (unpublished saves included); `--switch`: there, and this workspace tracks it, in one operation with no file changes. Names: Git's ref-name rules plus a portability policy; no collision with existing branches (ADR 0007 §5). Moves no other workspace or branch | shared branches (+ tracked branch with `--switch`) |
| `concurv branch list` | branches with head and the workspaces tracking them | no |
| `concurv branch switch <name>` | track another branch and check out its head, transactionally. Untracked files that do not collide stay. Refused, with nothing changed: unsaved changes, or untracked/ignored files that would be overwritten (20); a pending merge (43); an unknown branch (50); saves that would be on no branch (42). A failed switch is rolled back; an interrupted one is finished by running it again | working copy, workspace head and branch |
| `concurv undo` | reverse this workspace's most recent operation (`save`, `update`, `merge`, `restore`, `branch switch`); repeat to go further back. Refuses `publish` and `branch create` | workspace head and/or working copy |
| `concurv log [-n N]` | first-parent snapshot history of this workspace's head | no |
| `concurv restore --snapshot <id> [--force]` | make working files equal to the snapshot; history unchanged | working copy (+ backup snapshot, operation) |
| `concurv check-ignore <path>…` | explain whether each path is ignored (`.gitignore`, `.concurv/exclude`) or tracked, and by which rule; reports unusable patterns | no |
| `concurv verify [--full]` | integrity check of reachable graph (`--full`: every object file) | no |
| `concurv op log [-n N]` | operation log of all workspaces, newest first, with backups | no |
| `concurv git export <repo> [--from C] [--branch B]` | write concurv branch `C`'s history (default `main`) into a Git repository and move Git branch `B` (default: same name as `C`) by compare-and-swap. Requires `git` on PATH | the Git repository |
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
  `interrupted_restore` (null or `{target, backup, origin, branch}`; `origin` and `branch` are set for an interrupted `branch switch`: finish it with `branch switch <branch>`)
- `diff`: `files[]` (`path`, `kind`, `unified` or null, `note` or null)
- `save`: `created`, `snapshot`, `operation`, `changes[]`
- `publish`: `branch`, `from` (previous branch head or null), `to`, `published` (false if already there), `operation`
- `update`, `merge`: `branch` (the branch integrated), `result` ∈ `up_to_date|fast_forward|merged|conflicted|aborted`, `conflict` (pending-merge id or null), `conflicts[]` (`path`, `kind` ∈ `both_modified|both_added|delete_modify|file_directory`, `markers`, `side_files[]`), `content_merged[]` (paths merged line by line), for `aborted` also `backup`, `head`, `written`, `deleted`, `operation`
- `git export`: `branch`, `tip` (Git commit), `previous` (Git branch before, or null), `exported` (new commits), `reused` (already present), `updated`
- `git import`: `branch`, `git_tip`, `tip` (snapshot now at `main`), `commits`, `written`
- `undo`: `undone` (operation id), `undone_kind` ∈ `save|update|merge|restore|branch-switch`, `undone_description`, `head` (workspace head after), `files_restored_to` (snapshot or null if files were untouched), `written`, `deleted`, `operation`
- `branch create`: `branch`, `head`, `switched`, `operation`
- `branch list`: `branches[]` (`name`, `head`, `workspaces[]` (names), `current`)
- `branch switch`: `from` (or null), `branch`, `head`, `written`, `deleted`, `resumed` (finished an interrupted switch), `operation` (null if already on it)
- `workspace add`: `workspace` (as below), `written`
- `workspace list`: `workspaces[]` (`id`, `name`, `branch`, `head`, `root` (null if unknown on this device), `primary`, `current`)
- `log`: `snapshots[]` (`snapshot`, `parents[]`, `tree`, `change_id`, `author`, `timestamp_ms`, `message`)
- `restore`: `target`, `backup`, `written`, `deleted`, `operation`
- `check-ignore`: `paths[]` (`input`, `path`, `is_dir`, `tracked`, `ignored`, `rule` (null or `{source, pattern, negated}`), `excluded_parent` (null or a directory)), `unusable_patterns[]` (`source`, `line`, `pattern`, `reason`)
- `verify`: `full`, `operations`, `conflicts`, `snapshots`, `trees`, `blobs`, `unreachable`, `problems[]` (`code`, `subject`, `message`), `notes[]`
- `op log`: `operations[]` (`operation`, `parents[]`, `kind` ∈ `init|save|restore|workspace-add|publish|update|undo|merge-abort|git-import|branch-create|branch-switch|merge`, `description`, `timestamp_ms`, `device`, `workspace` (null for `init`), `backup` (null unless `restore --force` replaced unsaved work), `undoes` (for `undo`: the reversed operation), `checked_out` (snapshot the working copy was set to, or null), `head` (the **current** workspace's head after the operation, or null), `branches` (object: name → snapshot id))

## Error codes and exit codes

| Exit | Code | Meaning |
|---|---|---|
| 0 | — | success |
| 1 | `CONCURV_IO`, `CONCURV_INTERNAL` | OS error / bug |
| 2 | `CONCURV_INVALID_ARGUMENT` (and clap usage errors) | bad input (e.g. empty message, bad workspace name, publish without a saved snapshot, undo of a `publish`) |
| 10 | `CONCURV_NOT_A_REPO` | no repository found, or a workspace whose repository is gone |
| 11 | `CONCURV_ALREADY_INITIALIZED` | `init` on existing repo |
| 12 | `CONCURV_UNSUPPORTED_FORMAT` | unknown object/repository format version |
| 20 | `CONCURV_DIRTY_WORKSPACE` | restore/update/merge/undo/branch switch would discard unsaved changes, or would overwrite an ignored untracked file (also with `--force`); `branch switch` would overwrite or remove an untracked file |
| 21 | `CONCURV_CONCURRENT_MODIFICATION` | a file changed while concurv was reading/replacing it |
| 22 | `CONCURV_INVALID_PATH` | name violates concurv or platform path rules (incl. a `.git` entry on restore); nested workspace |
| 23 | `CONCURV_UNSUPPORTED_FILE_TYPE` | FIFO/socket/device, or link on Windows; `git import` of something concurv cannot represent (submodule, `.concurv` path, non-UTF-8 message) |
| 30 | `CONCURV_OBJECT_CORRUPT`, `CONCURV_WRONG_OBJECT_KIND` | integrity failure |
| 31 | `CONCURV_OBJECT_MISSING` | referenced object absent |
| 33 | `CONCURV_LIMIT_EXCEEDED` | input exceeds a hard limit |
| 40 | `CONCURV_LOCKED` | a repository or workspace lock stayed busy for the whole wait |
| 41 | `CONCURV_STALE_HEAD` | op head or workspace head moved during compare-and-swap |
| 42 | `CONCURV_DIVERGED` | `publish` refused: the branch has work this workspace has not integrated (run `update`); `git export` refused: the Git branch has commits outside the exported branch's history; `branch switch` refused: saves in this workspace are on neither the old nor the new branch |
| 43 | `CONCURV_CONFLICT` | `update`/`merge` refused: conflicting changes (without `--allow-conflicts`), file/directory conflict, ambiguous merge base, or a merge already pending; `save` refused: unresolved conflicts; `branch switch` and `branch create --switch` refused during a pending merge. Nothing modified |
| 50 | `CONCURV_NOT_FOUND` | id/prefix not found or not a snapshot; workspace not registered; nothing to undo; unknown branch (`branch switch`, `merge`, `workspace add --branch`, `git export --from`) |
| 51 | `CONCURV_AMBIGUOUS_ID` | prefix matches several objects |
| 60 | `CONCURV_EXTERNAL_TOOL` | `git` is not installed or a git command failed |
