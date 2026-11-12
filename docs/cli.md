# `concurv` CLI contract

Only the commands below exist. Anything else is a usage error (exit 2).
Every command runs in the workspace containing the current directory (or `-C`).

| Command | Effect | Mutates |
|---|---|---|
| `concurv init` | create `.concurv` in the current directory (atomic); it becomes workspace `default` | creates repo |
| `concurv status [--no-cache]` | changes of working copy vs this workspace's head; warns about interrupted restores. Uses the stat cache on Unix (ADR 0005); `--no-cache` hashes every file | no |
| `concurv diff [--no-cache]` | unified text diffs (Git style, incl. `\ No newline at end of file`; CRLF kept); binary/large/symlink changes are summarized | no |
| `concurv save -m <msg>` | durably snapshot the working copy and advance **this workspace's head**. Does not publish | workspace history |
| `concurv publish [--check]` | CAS fast-forward of the branch this workspace tracks (default `main`) to the workspace head. A branch protected by the `concurv-policy.json` of its current head also needs a passing attestation for exactly this head (RFC 0005), else exit 44; `--check` runs `check` first | shared branch |
| `concurv check [--branch B] [--trust]` | run the governing policy's checks on this workspace's head, in a fresh checkout of the snapshot (never the working copy), with a cleared environment and timeouts, and record an attestation (pass or fail). Refused (45) unless this policy version was approved on this device; `--trust` approves it. The first attestation upgrades the repository to format 3 | attestation objects (+ format) |
| `concurv update [--allow-conflicts \| --abort]` | bring the **tracked** branch into this workspace: fast-forward, or a three-way merge into a new snapshot (file level, plus line-level merge of text files whose edits do not overlap, ADR 0006). `--allow-conflicts`: write conflicts into the working copy and start a pending merge; `--abort`: give a pending merge up | working copy, workspace head |
| `concurv merge <branch> [--allow-conflicts] \| --abort` | integrate **another** branch, or a remote-tracking reference `remote/<remote>/<branch>` (RFC 0003 §5), with the rules of `update`; the workspace keeps its branch, and only a later `publish` (CAS) publishes the result. Merging the tracked branch is refused (use `update`). `--abort` = `update --abort` (ADR 0007) | working copy, workspace head |
| `concurv workspace add <path> [--name N] [--branch B]` | create a workspace in a new or empty directory, tracking `B` (default: the current workspace's branch), and check out that branch there | creates workspace |
| `concurv workspace list` | all workspaces with branch, head and path | no |
| `concurv workspace remove <name>` | unregister another workspace; its directory and files stay untouched (plain files afterwards). Refused, with nothing changed: the primary workspace or the current one (2); unsaved changes or an interrupted restore in its directory (20); a pending merge there (43); saves that no branch, tag, other workspace or remote-tracking reference contains (42). Holds that workspace's lock meanwhile | workspaces |
| `concurv branch create <name> [--at <branch\|tag\|snapshot> \| --switch]` | add a branch, by default at this workspace's head (unpublished saves included); `--switch`: there, and this workspace tracks it, in one operation with no file changes. Names: Git's ref-name rules plus a portability policy; no collision with existing branches (ADR 0007 §5). Moves no other workspace or branch | shared branches (+ tracked branch with `--switch`) |
| `concurv branch list` | branches with head and the workspaces tracking them | no |
| `concurv branch switch <name>` | track another branch and check out its head, transactionally. Untracked files that do not collide stay. Refused, with nothing changed: unsaved changes, or untracked/ignored files that would be overwritten (20); a pending merge (43); an unknown branch (50); saves that would be on no branch (42). A failed switch is rolled back; an interrupted one is finished by running it again | working copy, workspace head and branch |
| `concurv branch delete <name>` | remove a branch name; its snapshots stay and the output prints its head. Refused, with nothing changed: a workspace tracks it (2: switch it first); its head's policy protects it (44); its head is contained in no other branch, tag, workspace head or remote-tracking reference (42: merge it or `tag create` it first). Local only: a relay keeps its branch, and `fetch` does not recreate it (ADR 0007 §9) | shared branches |
| `concurv branch rename <old> <new>` | rename a branch; its head does not move, and every workspace tracking it tracks the new name in the same operation (no file changes). The new name follows the rules of new names (a change of case only is allowed). Refused for protected branches (44). Remote-tracking references keep the relay's name | shared branches, tracked branch of workspaces |
| `concurv undo` | reverse this workspace's most recent operation (`save`, `update`, `merge`, `restore`, `branch switch`); repeat to go further back. Refuses `publish`, `branch create/delete/rename` and tag operations | workspace head and/or working copy |
| `concurv log [-n N]` | first-parent snapshot history of this workspace's head | no |
| `concurv restore --snapshot <id> [--force]` | make working files equal to the snapshot; history unchanged | working copy (+ backup snapshot, operation) |
| `concurv check-ignore <path>…` | explain whether each path is ignored (`.gitignore`, `.concurv/exclude`) or tracked, and by which rule; reports unusable patterns | no |
| `concurv verify [--full]` | integrity check of reachable graph (`--full`: every object file) | no |
| `concurv op log [-n N]` | operation log of all workspaces, newest first, with backups | no |
| `concurv git export <repo> [--from C] [--branch B] [--tags]` | write concurv branch `C`'s history (default `main`) into a Git repository and move Git branch `B` (default: same name as `C`) by compare-and-swap. `--tags`: also the tags naming snapshots of that history, as lightweight `refs/tags/<name>`, in one reference transaction with the branch; a Git tag of the same name at another commit refuses the export (42) and no Git reference changes. Requires `git` on PATH | the Git repository |
| `concurv git import <repo> [--branch B] [--into C] [--tags]` | import Git branch `B` (default `main`) into concurv branch `C` (default `main`): commits concurv already has (`.concurv/git-map`, from earlier imports and exports) are reused, new ones become snapshots. `C` is created, or moved forward only if `B` contains its head (else 42: import `--into` a new branch and `merge` it). This workspace is checked out to the new head only if it tracks `C`, was at its old head and has no unsaved changes. `--tags`: Git tags naming commits of that history become concurv tags (annotated ones lose message and tagger); a concurv tag of the same name elsewhere refuses the import (42). All or nothing | history, branch `C`, tags, working copy (see above) |
| `concurv remote add <name> <url> --fingerprint <fp> [--token-file F]` | add a relay (`concurv://host[:port]`), pinning its certificate fingerprint; the device token comes from `F` or `$CONCURV_REMOTE_TOKEN` and is stored in `.concurv/remotes/<name>/token` (owner-only on Unix). Contacts nothing; upgrades the repository to format 2 (RFC 0001 §7) | repository config (+ format) |
| `concurv remote list` | remotes with URL, fingerprint and remote-tracking references | no |
| `concurv remote remove <name>` | delete the remote's configuration and token and, in one operation, its remote-tracking references (no local branch changes) | config, view |
| `concurv fetch [<remote>]` | download the relay's published branches (RFC 0003 §5): objects are verified and checked for completeness before anything is stored; remote-tracking references `remote/<remote>/<b>` move to the relay's heads; local branches that are behind fast-forward, branches new to this repository are created (not ones deleted or renamed here); diverged local branches stay. Never changes working files or workspace heads. A relay whose head went backwards is reported, not followed. The relay's tags that are missing here are created (valid names, fetched snapshots); a local tag is never moved (ADR 0008 §7). Default remote: the configured `remote`, the only one, else `origin` | objects, remote-tracking refs, local branches (fast-forward only), new tags |
| `concurv push [<remote>] [--branch B] [--tags]` | publish local branch `B` (default: the tracked branch) at the relay by compare-and-swap, only as a fast-forward of the relay's head; refused (42) if the relay has work this repository has not fetched or that diverged; a lost connection is retried and never publishes twice. If the policy in the relay's head protects the branch, a recorded passing attestation for the head is uploaded and vouched for (the device needs scope `attest`; else 74, or 44 without one). `--tags`: also create this repository's tags at the relay, for snapshots the relay has (others are listed); a relay tag of the same name elsewhere refuses all of them (42); tags never move (ADR 0008 §7) | the relay's branch, remote-tracking ref (+ relay tags) |
| `concurv clone <url> <dir> --fingerprint <fp> [--token-file F] [--name origin]` | `init` in `<dir>` (missing or empty), `remote add`, `fetch`, then check out `main` if the relay has it | creates repo |
| `concurv intent declare <pattern>… [--symbol S]… [--ttl 1h] [--note T]` | advisory: tell other workspaces and agents which paths (ignore-file patterns) or symbols this workspace is about to change, for 1 minute to 7 days (RFC 0006). Never a lock; expired intents are ignored everywhere. `push`/`fetch` exchange this device's intents with the relay | repository intents |
| `concurv intent list` | active intents of this repository and those fetched from relays | no |
| `concurv intent release [ID]` | remove this workspace's intents (all, or by id prefix) | repository intents |
| `concurv overlaps` | warnings, never refusals: files this workspace changed that another workspace's unpublished saves or upstream work also change (`path`), where the line edits would conflict (`conflict`), that match another's intent (`intent`), or that use a symbol whose definition changed upstream (`dependency`) | no |
| `concurv events [-n N]` | the operation log as versioned JSON lines, oldest first (with `--json`: an `events` array) | no |
| `concurv tag create <name> [--at <branch\|tag\|snapshot>]` | name a snapshot (default: this workspace's head) with a tag that never moves (ADR 0008). Names: the rules of new branch names, no collision with another tag. An existing name is refused. The first tag upgrades the repository to format 4 | shared tags (+ format) |
| `concurv tag list` | tags with their snapshots | no |
| `concurv tag delete <name>` | remove the name; the snapshot stays and is printed (`tag create <name> --at <id>` restores it). `undo` refuses tag operations | shared tags |
| `concurv config get <key>` | the effective value of `author` or `remote` and its source (`environment`, `repository`, `user`); not set: exit 50 | no |
| `concurv config set <key> <value> [--global]` | write the repository file `.concurv/config.json` (shared by its workspaces), or with `--global` the user file. Unknown keys and invalid values are refused | configuration file |
| `concurv config unset <key> [--global]` | remove a key from that file | configuration file |
| `concurv config list` | every known key with its effective value and source, and both file paths | no |
| `concurv completions <shell>` | print a completion script for `bash`, `zsh`, `fish`, `powershell` or `elvish` | no |

Global flags: `--json`, `-C <dir>`. Sync commands use the token stored for the remote,
or `CONCURV_REMOTE_TOKEN` if set. Author: `CONCURV_AUTHOR`, else `author` from the
repository configuration, else from the user configuration, else the OS user name
(`USER`/`USERNAME`), else `unknown`. Default remote for `fetch`/`push`: the one named,
else the configured `remote`, else the only remote, else `origin`.

**Configuration** (since R6): the repository file is `.concurv/config.json`; the
user file is `$CONCURV_CONFIG` if set (empty: none), else
`%APPDATA%\concurv\config.json` on Windows and `$XDG_CONFIG_HOME/concurv/config.json`
(default `~/.config/concurv/config.json`) elsewhere. Format:
`{"concurv_config": 1, "settings": {"author": "…", "remote": "…"}}`. A malformed
file, an unknown key or an invalid value stops every command (exit 2, naming the
file) except `completions`; nothing is guessed. Writes are atomic, under the
repository lock (repository file) or `config.lock` next to the user file.

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
- `publish`: `branch`, `from` (previous branch head or null), `to`, `published` (false if already there), `operation`, `attestation` (the attestation that authorized a protected publish, or null)
- `check`: `attestation`, `passed`, `candidate`, `branch`, `policy` (blob id), `environment` (`os`, `arch`, `concurv`), `checks[]` (`name`, `argv[]`, `exit_code` (−1000001: timeout, −1000002: could not start), `duration_ms`, `output` (blob id of combined stdout+stderr)), `newly_trusted`
- `update`, `merge`: `branch` (the branch integrated), `result` ∈ `up_to_date|fast_forward|merged|conflicted|aborted`, `conflict` (pending-merge id or null), `conflicts[]` (`path`, `kind` ∈ `both_modified|both_added|delete_modify|file_directory`, `markers`, `side_files[]`), `content_merged[]` (paths merged line by line), for `aborted` also `backup`, `head`, `written`, `deleted`, `operation`
- `git export`: `branch`, `tip` (Git commit), `previous` (Git branch before, or null), `exported` (new commits), `reused` (already present), `updated`, `tags[]` (with `--tags`: `name`, `commit`, `created` (false if Git already had it)), `tags_skipped[]` (names of tags outside the exported history)
- `git import`: `branch`, `into`, `git_tip`, `tip` (snapshot now at `into`), `commits` (newly imported), `published` (`into` created or moved), `checked_out`, `written`, `tags_created[]` (`name`, `target`), `tags_unchanged`, `tags_skipped[]` (`name`, `reason`), `annotated_dropped`
- `tag create`: `tag`, `target`, `operation`, `format_upgraded`
- `tag list`: `tags[]` (`name`, `target`)
- `tag delete`: `tag`, `target` (where it was), `operation`
- `config get`: `key`, `value`, `source` ∈ `environment|repository|user`
- `config set`: `key`, `value`, `scope` ∈ `repository|user`, `previous` (or null)
- `config unset`: `key`, `scope`, `previous` (null if it was not set)
- `config list`: `keys[]` (`key`, `value` (or null), `source` (or null), `description`), `repository_file` (null outside a repository), `user_file` (or null)
- `completions`: `shell`, `script`
- `undo`: `undone` (operation id), `undone_kind` ∈ `save|update|merge|restore|branch-switch`, `undone_description`, `head` (workspace head after), `files_restored_to` (snapshot or null if files were untouched), `written`, `deleted`, `operation`
- `branch create`: `branch`, `head`, `switched`, `operation`
- `branch list`: `branches[]` (`name`, `head`, `workspaces[]` (names), `current`)
- `branch switch`: `from` (or null), `branch`, `head`, `written`, `deleted`, `resumed` (finished an interrupted switch), `operation` (null if already on it)
- `branch delete`: `branch`, `head` (where it was), `operation`
- `branch rename`: `branch` (old name), `new_name`, `head`, `workspaces[]` (names now tracking the new name), `operation`
- `workspace add`: `workspace` (as below), `written`
- `workspace list`: `workspaces[]` (`id`, `name`, `branch`, `head`, `root` (null if unknown on this device), `primary`, `current`)
- `workspace remove`: `workspace` (as in `workspace list`, before removal), `directory_checked` (its directory was found and checked), `operation`
- `log`: `snapshots[]` (`snapshot`, `parents[]`, `tree`, `change_id`, `author`, `timestamp_ms`, `message`)
- `restore`: `target`, `backup`, `written`, `deleted`, `operation`
- `check-ignore`: `paths[]` (`input`, `path`, `is_dir`, `tracked`, `ignored`, `rule` (null or `{source, pattern, negated}`), `excluded_parent` (null or a directory)), `unusable_patterns[]` (`source`, `line`, `pattern`, `reason`)
- `verify`: `full`, `operations`, `conflicts`, `snapshots`, `trees`, `blobs`, `unreachable`, `problems[]` (`code`, `subject`, `message`), `notes[]`
- `op log`: `operations[]` (`operation`, `parents[]`, `kind` ∈ `init|save|restore|workspace-add|publish|update|undo|merge-abort|git-import|branch-create|branch-switch|merge|fetch|push|remote-remove|tag-create|tag-delete|branch-delete|branch-rename|workspace-remove`, `description`, `timestamp_ms`, `device`, `workspace` (null for `init`), `backup` (null unless `restore --force` replaced unsaved work), `undoes` (for `undo`: the reversed operation), `checked_out` (snapshot the working copy was set to, or null), `head` (the **current** workspace's head after the operation, or null), `branches` (object: name → snapshot id), `remotes` (object: remote → branch → `{head, seq}`; since R3), `tags` (object: name → snapshot id; since R6))
- `remote add`: `name`, `url`, `fingerprint`
- `remote list`: `remotes[]` (`name`, `url`, `fingerprint`, `branches` (object: branch → `{head, seq}`, the remote-tracking references))
- `remote remove`: `name`, `operation` (null if it had no remote-tracking references)
- `fetch`: `remote`, `relay` (relay id), `objects` (received), `branches[]` (`branch`, `remote_head` (null if `vanished`), `seq`, `tracked_before`, `local_before`, `local_after`, `action` ∈ `up_to_date|created|fast_forwarded|local_ahead|diverged|rewound|vanished|invalid_name|name_collision|not_local`, `note`), `tags[]` (the relay's tags: `name`, `target`, `action` ∈ `created|unchanged|conflict|missing_target|invalid`, `note`), `operation` (null if nothing changed)
- `push`: `remote`, `branch`, `from` (relay head before, or null), `head` (local branch head), `seq` (relay sequence number after, if known), `result` ∈ `pushed|up_to_date|behind`, `objects` (uploaded), `replayed` (the relay confirmed an earlier attempt whose reply was lost), `operation`, `tags` (null without `--tags`; else `created[]` (`name`, `target`), `existing`, `skipped[]` (`name`, `reason`))
- `clone`: the `fetch` fields plus `root`, `checked_out` (snapshot or null), `written`
- `intent declare`: the intent (`concurv_intent`, `id`, `device`, `workspace`, `who`, `paths[]`, `symbols[]`, `note`, `created_ms`, `expires_ms`)
- `intent list`: `intents[]` (as above)
- `intent release`: `released`
- `overlaps`: `overlaps[]` (`level` ∈ `path|conflict|intent|dependency`, `source` (`workspace <name>`, `branch <b>`, `remote/<r>/<b>` or `intent <id> (<who>)`), `path`, `symbol` (null unless `intent` or `dependency`))
- `events`: `events[]`, each `{concurv_event: 1, type: "operation", operation, kind, time_ms, device, workspace (or null), branch_moves (branch → {from, to}), head, description}`; without `--json`, one such object per line

## Error codes and exit codes

| Exit | Code | Meaning |
|---|---|---|
| 0 | — | success |
| 1 | `CONCURV_IO`, `CONCURV_INTERNAL` | OS error / bug |
| 2 | `CONCURV_INVALID_ARGUMENT` (and clap usage errors) | bad input (e.g. empty message, bad workspace name, publish without a saved snapshot, undo of a `publish`) |
| 10 | `CONCURV_NOT_A_REPO` | no repository found, or a workspace whose repository is gone |
| 11 | `CONCURV_ALREADY_INITIALIZED` | `init` on existing repo |
| 12 | `CONCURV_UNSUPPORTED_FORMAT` | unknown object/repository format version (for example a format-4 repository, which has tags, opened by a build older than R6) |
| 20 | `CONCURV_DIRTY_WORKSPACE` | restore/update/merge/undo/branch switch would discard unsaved changes, or would overwrite an ignored untracked file (also with `--force`); `branch switch` would overwrite or remove an untracked file |
| 21 | `CONCURV_CONCURRENT_MODIFICATION` | a file changed while concurv was reading/replacing it |
| 22 | `CONCURV_INVALID_PATH` | name violates concurv or platform path rules (incl. a `.git` entry on restore); nested workspace |
| 23 | `CONCURV_UNSUPPORTED_FILE_TYPE` | FIFO/socket/device, or link on Windows; `git import` of something concurv cannot represent (submodule, `.concurv` path, non-UTF-8 message) |
| 30 | `CONCURV_OBJECT_CORRUPT`, `CONCURV_WRONG_OBJECT_KIND` | integrity failure |
| 31 | `CONCURV_OBJECT_MISSING` | referenced object absent |
| 33 | `CONCURV_LIMIT_EXCEEDED` | input exceeds a hard limit |
| 40 | `CONCURV_LOCKED` | a repository or workspace lock stayed busy for the whole wait |
| 41 | `CONCURV_STALE_HEAD` | op head or workspace head moved during compare-and-swap |
| 42 | `CONCURV_DIVERGED` | `publish` refused: the branch has work this workspace has not integrated (run `update`); `git export` refused: the Git branch has commits outside the exported branch's history, or (`--tags`) Git has a tag of the same name at another commit; `git import` refused: the concurv branch has snapshots the Git branch lacks, or (`--tags`) a concurv tag of the same name is elsewhere; `branch switch` refused: saves in this workspace are on neither the old nor the new branch; `branch delete` refused: no other reference contains the branch's head |
| 43 | `CONCURV_CONFLICT` | `update`/`merge` refused: conflicting changes (without `--allow-conflicts`), file/directory conflict, ambiguous merge base, or a merge already pending; `save` refused: unresolved conflicts; `branch switch` and `branch create --switch` refused during a pending merge. Nothing modified |
| 44 | `CONCURV_POLICY_FAIL` | a protected branch (RFC 0005) refused the publish or push: no recorded passing attestation for exactly this candidate under the current policy (stale, failing, other branch, other OS), an invalid policy, or a policy that excludes this OS for `check`; `branch delete` or `branch rename` of a branch its policy protects |
| 45 | `CONCURV_UNTRUSTED_POLICY` | `check`: this version of the branch policy has not been approved on this device (`--trust`); nothing ran |
| 50 | `CONCURV_NOT_FOUND` | id/prefix not found or not a snapshot; workspace not registered; nothing to undo; unknown branch (`branch switch`, `merge`, `workspace add --branch`, `git export --from`, `push`); unknown remote |
| 51 | `CONCURV_AMBIGUOUS_ID` | prefix matches several objects; `--at` names a branch and a tag that point at different snapshots |
| 60 | `CONCURV_EXTERNAL_TOOL` | `git` is not installed or a git command failed |
| 70 | `CONCURV_NETWORK` | the relay could not be reached, or the connection broke or timed out (sync commands; nothing was changed locally unless the output says so) |
| 71 | `CONCURV_PROTOCOL` | a peer sent a malformed or unexpected message |
| 72 | `CONCURV_PROTOCOL_MISMATCH` | client and relay share no protocol version |
| 73 | `CONCURV_AUTH_FAILED` | the relay refused the device token (unknown, malformed or revoked) |
| 74 | `CONCURV_PERMISSION_DENIED` | the device lacks the scope (`read`, `write`, `publish:<branch>`) |
| 75 | `CONCURV_RELAY_IDENTITY` | the relay's certificate does not match the pinned fingerprint; nothing, not even the token, was sent |

Sync adds meanings to existing codes: 41 `CONCURV_STALE_HEAD` — the relay's
branch moved between your fetch and your push (fetch, integrate, push
again); 42 `CONCURV_DIVERGED` — `push` refused because the relay has work
this repository has not fetched, the branches diverged, or the relay's head
went backwards; 30/31 — a peer sent a corrupt or incomplete history
(nothing was stored); 12 — a repository of format 2 opened by a build
older than R3.

## `concurvd` (the relay)

| Command | Effect |
|---|---|
| `concurvd init <dir>` | create a relay in a new or empty directory: object store, empty state and device registry, a new TLS identity; prints the fingerprint clients pin |
| `concurvd fingerprint <dir>` | print the fingerprint |
| `concurvd device add <dir> <name> [--scope S]…` | register a device and print its token once (default scopes: `read`, `write`, `publish:*`; `publish:<branch>` limits publishing to one branch; `attest` lets it vouch for attestations required by protected branches, RFC 0005 §7) |
| `concurvd device list <dir>` | devices, scopes, revocation |
| `concurvd device revoke <dir> <name>` | the token stops working at the next connection |
| `concurvd serve <dir> [--listen ADDR] [--max-connections N] [--idle-timeout SECS]` | serve until stopped (default `127.0.0.1:7461`; port 0 picks a free port). The first line of output is `listening on <addr>` (with `--json`: `data.listening`) |

`concurvd --json` prints the same envelope as `concurv --json`. `data`:
`init` `dir`, `relay`, `fingerprint`; `fingerprint` `fingerprint`; `device
add` `device`, `scopes`, `token`; `device list` `devices[]` (`name`,
`scopes`, `revoked`, `created_ms`); `device revoke` `device`, `revoked`.
A relay can be stopped (or killed) at any moment: its state is replaced
atomically, so it restarts with the last committed heads.
