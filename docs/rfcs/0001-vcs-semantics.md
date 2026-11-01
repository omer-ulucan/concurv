# RFC 0001 — VCS semantics, identifiers and canonical encoding

Status: **Accepted for R1** (format version 1). Changes to anything marked
*normative* require a new format version and a migration note in ADR 0001.

## 1. Entities

| Entity | Identity | Mutability | R1 status |
|---|---|---|---|
| Blob | `BlobId` = content hash | immutable | implemented |
| Tree | `TreeId` = content hash | immutable | implemented |
| Snapshot | `SnapshotId` = content hash | immutable | implemented |
| Operation | `OperationId` = content hash | immutable | implemented |
| Change | `ChangeId` = 16 random bytes | logical identity | recorded in every snapshot, not yet used for rewriting |
| Workspace | `WorkspaceId` = 16 random bytes | mutable files on disk | one workspace per repo in R1 |
| Device | `DeviceId` = 16 random bytes | stable per repository clone | recorded in operations |
| Branch | name (UTF-8) | mutable pointer inside an Operation's view | `main` only in R1 |
| Local head | `View.workspaces[w].head` | mutable via operation | implemented |
| Published head | authoritative CAS ref on a `concurvd` relay | — | **R3, not implemented** |
| Candidate / proposal | Snapshot produced by integration | immutable | **R4, not implemented** |
| Validation attestation | object binding candidate hash + policy + env | immutable | **R4, not implemented** |

Definitions:

- **Blob** — exact file bytes (including empty and binary content) or the raw
  target bytes of a symbolic link. No newline or encoding normalization.
- **Tree** — one directory level: a strictly sorted list of `(name, kind, id)`.
- **Snapshot** — a tree plus parents, a ChangeId, author, timestamp and message.
  A snapshot represents a materializable code state. Parents are ordered
  (first parent is the mainline).
- **Change** — a logical unit of work. In R1 every `concurv save` creates a new
  ChangeId. Later stages (R2+) may carry a ChangeId across rewritten snapshots.
  A shared ChangeId is *not* a claim that two snapshots' patches commute.
- **Operation** — an append-only record of one repository mutation (`init`,
  `save`, `restore`). Each operation stores its parent operation ids and the
  complete resulting **View** (all branch and workspace pointers). Operation
  history is independent of snapshot history, enabling precise
  workspace-scoped undo later (R2).
- **View** — the full mutable state of the repository, as of one operation:
  `branches: name → SnapshotId` and `workspaces: WorkspaceId → {branch, head}`.
- **Workspace** — a private checkout. Its working files are *never* shared and
  never versioned until `concurv save` acknowledges them.
- **Op head** — the single mutable file `.concurv/op_head` naming the current
  operation. **It is the only mutable pointer in an R1 repository**; all branch
  and workspace refs are derived from the operation it names. This makes every
  repository mutation a single atomic pointer swap (see RFC 0002).

## 2. Identifier scheme (normative)

All content-addressed ids are 32-byte BLAKE3 outputs computed in
**derive-key mode** with the context string

```
"concurv 2026-11 object v1"
```

over the object's complete canonical encoding (§3), which begins with a kind
byte. This gives two layers of domain separation: (a) concurv hashes can never
collide with plain BLAKE3 hashes or other concurv hash uses, (b) a blob and a tree
with identical payload bytes have different ids. The Rust types `BlobId`,
`TreeId`, `SnapshotId` and `OperationId` are distinct types; loading an object
through the wrong typed API fails with `CONCURV_WRONG_OBJECT_KIND`.

Textual form: 64 lowercase hex characters. CLI input accepts a unique prefix of
at least 8 hex characters.

ChangeId, WorkspaceId and DeviceId are 16 bytes from the OS CSPRNG, hex
encoded (32 chars). They are not content hashes and are never accepted where an
ObjectId is expected.

concurv ids are **not** Git ids. See ADR 0002.

## 3. Canonical object encoding (normative)

All integers are **big-endian**. Every object is:

```
offset  size  field
0       4     magic  = "CNV1" (0x43 0x4e 0x56 0x31)
4       1     kind   : 1 = blob, 2 = tree, 3 = snapshot, 4 = operation
5       8     payload_len : u64
13      N     payload (exactly payload_len bytes)
```

Decoders must reject: wrong magic, unknown kind, a kind different from the one
requested, a length that disagrees with the actual byte count (truncated or
trailing bytes), and payloads exceeding limits (§5). Unknown magic versions are
reported as `CONCURV_UNSUPPORTED_FORMAT`, never parsed best-effort.

Strings below are `u32 length || UTF-8 bytes`. `Id` is 32 raw bytes, `Rid`
(random id) is 16 raw bytes.

### 3.1 Blob payload

Raw bytes, unmodified.

### 3.2 Tree payload

```
u32 entry_count
repeat entry_count:
  u8   entry_kind : 1 = file, 2 = executable file, 3 = symlink, 4 = tree
  u16  name_len
  name bytes (UTF-8)
  Id   target  (blob for kinds 1–3, tree for kind 4)
```

- Entries are sorted by **raw name bytes, strictly ascending** (memcmp order).
  Duplicates or unsorted input are a decode error, so every tree has exactly
  one encoding.
- Names must satisfy §4.
- An empty tree is valid only as the root (an empty directory is not
  versioned, matching Git; empty sub-trees are rejected by the encoder and
  decoder).

### 3.3 Snapshot payload

```
Id     tree
u32    parent_count, then parent_count × Id  (ordered, no duplicates)
Rid    change_id
i64    timestamp_ms  (Unix epoch, UTC)
String author
String message
```

### 3.4 Operation payload

```
u32    parent_count, then parent_count × Id (operation ids, no duplicates)
i64    timestamp_ms
Rid    device_id
Rid    workspace_id     (the actor; all-zero bytes = repository-level)
String kind             ("init" | "save" | "restore")
String description
u8     has_backup; if 1: Id backup_snapshot
View   view
```

View:

```
u32 branch_count;  repeat: String name, Id snapshot   (sorted by name bytes, unique)
u32 workspace_count; repeat:                          (sorted by id bytes, unique)
  Rid  workspace_id
  u8   has_branch; if 1: String branch
  u8   has_head;   if 1: Id snapshot
```

## 4. Paths and file kinds (normative)

A tree entry name must be: non-empty, ≤ 255 bytes, valid UTF-8, not `.` or
`..`, and contain no `/`, `\`, or NUL. The name `.concurv` (compared
ASCII-case-insensitively) is rejected at **every** level, so concurv metadata can
never be materialized. The scanner skips directories named `.concurv` at any level
(and `.concurv-init-*` staging directories at the root) instead of versioning them.

Snapshot rules (`concurv save`):

| On disk | Recorded as |
|---|---|
| regular file | `file`, or `executable` if any execute bit is set (Unix) |
| directory | `tree` (omitted if it contains nothing versionable) |
| symlink (Unix) | `symlink`; blob = raw target bytes; never followed |
| symlink / junction / reparse point (Windows) | **rejected** with `CONCURV_UNSUPPORTED_FILE_TYPE` |
| FIFO, socket, device | **rejected** with `CONCURV_UNSUPPORTED_FILE_TYPE` |
| non-UTF-8 name | **rejected** with `CONCURV_INVALID_PATH` |

On Windows there is no executable bit: a file keeps the kind (file vs
executable) it had in the workspace head snapshot; new files are `file`.

Restore rules: concurv never writes through a symlink or outside the workspace
root; every parent directory is checked with `symlink_metadata` before writing.
On Windows and macOS, restore refuses trees whose sibling names collide under
case folding, and on Windows it refuses names that are reserved (`CON`, `NUL`,
`COM1`…, with any extension), contain `<>:"|?*` or control characters, end
in a dot or space, or look like an NTFS 8.3 alias of the metadata directory
(`CONCUR~1`, `CONCUR~2`, …). These refusals happen **before** any file is modified.

Known limits: Unicode normalization is not applied (macOS HFS+ NFD rewriting
of names is untested); on Windows a restored `executable` entry is written as
a plain file and the next save keeps the kind recorded in the workspace head.

Rename detection is not part of the model in R1: a rename is a delete plus an
add.

## 5. Limits

| Limit | Value |
|---|---|
| Name length | 255 bytes |
| Tree entries | 1,000,000 |
| Non-blob object payload | 64 MiB |
| Blob size | u64 (streamed, never fully buffered by save/restore/verify) |
| Snapshot parents / operation parents | 1024 |
| String field | 16 MiB |

A header announcing a larger non-blob payload is rejected before allocation.

## 6. Invariants enforced in R1

- **R1-I1** every object file's bytes hash to its file name; reads verify this.
- **R1-I2** an operation is only reachable from `op_head` after every object it
  references (transitively) is durable (RFC 0002).
- **R1-I3** `op_head` always names an existing, valid operation; mutations are
  serialized by an exclusive repository lock and `op_head` is updated with a
  compare-and-swap against the operation id read under that lock.
- **R1-I4** `status`, `diff`, `log` and `verify` never write to the repository or
  the working copy, and take no lock. Only mutating commands (`save`, `restore`)
  delete stale `.concurv/tmp` debris, and only while holding the lock.
- **R1-I5** `restore` never discards working-copy data that differs from the
  head snapshot unless `--force` is given, and with `--force` the current
  working copy is first saved as a durable backup snapshot recorded in the
  operation log.
- **R1-I6** branches never move backwards: `restore` rewrites working files
  only; recording the restored state is a new forward `save`.

## 7. Forward compatibility notes

- R2 (multiple workspaces) adds entries to `View.workspaces`; concurrent
  writers already serialize through the lock + CAS on `op_head`.
- R3 published refs live on a relay and are *not* stored in the local View as
  authoritative; a local `remote/<name>/<branch>` mirror may be added to View.
- New object kinds (conflict, attestation) get new kind bytes; old decoders
  reject them with `CONCURV_UNSUPPORTED_FORMAT` rather than misparse.

## 8. Test vectors (normative for format version 1)

An independent implementation must reproduce these ids exactly. They are
pinned in `crates/concurv-objects/tests/objects.rs::golden_hash_vectors` and
`crates/concurv-workspace/tests/engine.rs::deterministic_tree_ids_across_repositories`;
a change to any of them means the format changed and needs a new version.

| Input | Kind | Id |
|---|---|---|
| empty content | blob | `b7778e72e4763260833712ebc5ced9fe9b2bc8451b94f56ae6f7f5c74bed0413` |
| `hello\n` (6 bytes) | blob | `313293e1ac59b3b5d6f89c8821db96909f7a208b6b9cd038d5439e229ae1d10a` |
| `sample_tree()` in `objects.rs` (4 entries, fixed ids) | tree | `d3a356e496979b1c4b017344ae45283450c087aa0408bacc9b1af3feb1ca0515` |
| `sample_snapshot()` in `objects.rs` | snapshot | `039d5b0538da9f766b073a2439d9e30c2d74dac84d501ee143528ad58da1896d` |
| fixture working copy in `engine.rs` (6 files, nested dirs, Unicode name) | tree | `047aec882a9e5b6d02f539d60d303929c91666393a5ae47815c3acd9147b69c9` |

Worked example for the `hello\n` blob: hash, in BLAKE3 derive-key mode with
context `"concurv 2026-11 object v1"`, the 19 bytes

```
43 4e 56 31             magic "CNV1"
01                      kind = blob
00 00 00 00 00 00 00 06 payload_len = 6 (u64 big-endian)
68 65 6c 6c 6f 0a       "hello\n"
```

These vectors were produced on Windows 11 (NTFS). Reproduction on Linux and
macOS is an open R1 gate item (`docs/ROADMAP_STATUS.md`).
