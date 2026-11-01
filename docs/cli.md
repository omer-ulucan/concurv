# `arc` CLI contract (R1)

Only the commands below exist. Anything else is a usage error (exit 2).

| Command | Effect | Mutates |
|---|---|---|
| `arc init` | create `.arc` in the current directory (atomic) | creates repo |
| `arc status` | changes of working copy vs workspace head; warns about interrupted restores | no |
| `arc diff` | unified text diffs; binary/large/symlink changes are summarized | no |
| `arc save -m <msg>` | durably snapshot the working copy, advance head and branch `main` | history |
| `arc log [-n N]` | first-parent snapshot history of the head | no |
| `arc restore --snapshot <id> [--force]` | make working files equal to the snapshot; history unchanged | working copy (+ backup snapshot, operation) |
| `arc verify [--full]` | integrity check of reachable graph (`--full`: every object file) | no |

Global flags: `--json`, `-C <dir>`. Author comes from `ARC_AUTHOR`, else the OS
user name (`USER`/`USERNAME`), else `unknown`.

`restore` refuses a dirty working copy (exit 20). With `--force` it first
saves the working copy as a backup snapshot (printed, and recorded in the
operation log); recover with `arc restore --snapshot <backup> --force`.

## JSON envelope (`arc_json_version` 1)

```json
{ "arc_json_version": 1, "command": "status", "ok": true, "data": { ... } }
{ "arc_json_version": 1, "command": "status", "ok": false,
  "error": { "code": "ARC_NOT_A_REPO", "message": "...", "exit_code": 10 } }
```

`verify` with problems returns `ok: false` **and** `data` (the report).
Additive fields may appear in later versions; removing or changing the meaning
of a field requires bumping `arc_json_version`.

`data` per command:

- `init`: `root`, `branch`
- `status`: `branch`, `head` (hex or null), `clean`, `changes[]`
  (`path`, `kind` ∈ `added|deleted|modified|type_changed`, `old_blob`, `new_blob`),
  `interrupted_restore` (null or `{target, backup}`)
- `diff`: `files[]` (`path`, `kind`, `unified` or null, `note` or null)
- `save`: `created`, `snapshot`, `operation`, `changes[]`
- `log`: `snapshots[]` (`snapshot`, `parents[]`, `tree`, `change_id`, `author`, `timestamp_ms`, `message`)
- `restore`: `target`, `backup`, `written`, `deleted`, `operation`
- `verify`: `full`, `operations`, `snapshots`, `trees`, `blobs`, `unreachable`, `problems[]` (`code`, `subject`, `message`), `notes[]`

## Error codes and exit codes

| Exit | Code | Meaning |
|---|---|---|
| 0 | — | success |
| 1 | `ARC_IO`, `ARC_INTERNAL` | OS error / bug |
| 2 | `ARC_INVALID_ARGUMENT` (and clap usage errors) | bad input |
| 10 | `ARC_NOT_A_REPO` | no repository found |
| 11 | `ARC_ALREADY_INITIALIZED` | `init` on existing repo |
| 12 | `ARC_UNSUPPORTED_FORMAT` | unknown object/repository format version |
| 20 | `ARC_DIRTY_WORKSPACE` | restore would discard unsaved changes |
| 21 | `ARC_CONCURRENT_MODIFICATION` | a file changed while ARC was reading/replacing it |
| 22 | `ARC_INVALID_PATH` | name violates ARC or platform path rules |
| 23 | `ARC_UNSUPPORTED_FILE_TYPE` | FIFO/socket/device, or link on Windows |
| 30 | `ARC_OBJECT_CORRUPT`, `ARC_WRONG_OBJECT_KIND` | integrity failure |
| 31 | `ARC_OBJECT_MISSING` | referenced object absent |
| 33 | `ARC_LIMIT_EXCEEDED` | input exceeds a hard limit |
| 40 | `ARC_LOCKED` | another ARC process holds the repository lock |
| 41 | `ARC_STALE_HEAD` | op head moved during compare-and-swap |
| 50 | `ARC_NOT_FOUND` | id/prefix not found or not a snapshot |
| 51 | `ARC_AMBIGUOUS_ID` | prefix matches several objects |
