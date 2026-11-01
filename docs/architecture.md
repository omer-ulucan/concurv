# concurv architecture (as implemented in R1)

```
concurv-cli ──► concurv-workspace ──► concurv-ops ──► concurv-refs ──► concurv-objects ──► concurv-core
 (clap,      (repo, scan,      (OpLog:     (op_head     (encoding,      (ids, errors,
  JSON)       save, status,     commit,     CAS, OS      store, durable  path rules)
              diff, restore,    walk,       lock)        fs, faults)
              verify, log)      clean_tmp)
```

| Crate | Key types | Responsibility |
|---|---|---|
| `concurv-core` | `ObjectId`, `BlobId`/`TreeId`/`SnapshotId`/`OperationId`, `ChangeId`/`WorkspaceId`/`DeviceId`, `ConcurvError`, `ErrorCode`, `RepoPath`, `PlatformRules` | domain ids with compile-time separation; stable error/exit codes; name validation and Windows/case-folding rules |
| `concurv-objects` | `encoding::{Tree, Snapshot, Operation, View}`, `ObjectStore`, `fs::{TempFile, Faults}` | RFC 0001 byte format with strict decoders; verified loose-object store; durable write primitives with deterministic failpoints |
| `concurv-refs` | `OpHead`, `RepoLock` | the only mutable pointer and its CAS; exclusive OS lock |
| `concurv-ops` | `OpLog` | commit an operation (re-verify referenced snapshots, write op, CAS); walk history; clean temp debris |
| `concurv-workspace` | `Repo`, `FlatTree`, `save`, `status`, `diff`, `restore`, `verify`, `log` | everything touching the working copy |
| `concurv-cli` | `concurv` binary | argument parsing, human and JSON output |

## Key decisions

- **One mutable pointer.** Every operation object stores the complete View
  (branches + workspace heads). A mutation = write immutable objects → write
  operation → atomic rename of `op_head`. See ADR 0001, RFC 0002.
- **Content is the oracle.** `status` hashes every file (no stat cache), so a
  same-size/same-mtime edit cannot be missed. This costs O(repository bytes)
  per command.
- **Restore never rewrites history** and never discards unsaved data without
  `--force` + durable backup.
- **Synchronous, single-writer engine.** No async runtime; concurrency is
  serialized by the lock. Multi-workspace concurrency is R2.

## Data flow of `concurv save`

1. Take lock; delete `.concurv/tmp/*` debris.
2. Read op head → View → workspace head → head file map.
3. Walk working copy (no symlink following); stream each file into a temp
   object, fsync, rename into `objects/`, fsync dir (dedup verifies existing).
4. Build canonical trees bottom-up; write snapshot.
5. New View (branch + workspace head → snapshot); write operation; CAS op head.

## Not yet implemented

Multiple workspaces, undo, merge/conflicts, sync, Git bridge, GC/packing,
stat cache, ignore files (`.arcignore`), rename detection. See
`ROADMAP_STATUS.md`.
