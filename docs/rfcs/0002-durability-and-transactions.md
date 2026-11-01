# RFC 0002 — Durability, transactions and recovery

Status: **Accepted for R1**.

## 1. Acknowledgment contract

When `arc save` (or `init`, `restore`) exits successfully (exit code 0, or
`"ok": true` in JSON), ARC guarantees, **within the platform contract in §5**:

1. every object reachable from the new operation has been written, `fsync`ed,
   atomically renamed into the object store, and its directory `fsync`ed;
2. the new operation object is durable in the same way;
3. `.arc/op_head` has been atomically replaced to name the new operation and
   the `.arc` directory has been `fsync`ed.

If ARC exits with an error, or crashes, the repository is in **either** the
previous acknowledged state **or** the new one — never a state where `op_head`
names a missing or partially written object. Unreferenced objects left behind
by a failed transaction are harmless (they are immutable and content-addressed)
and will be reclaimable by a future GC (not implemented; see §7).

Not covered: unsaved editor buffers; working-copy files that were never saved;
disks or drivers that ignore flush requests; filesystems mounted with
durability-reducing options; simultaneous loss of every replica.

## 2. Repository layout

```
.arc/
  format          "arc-repo-format 1\n" — written by init, checked by open
  device_id       32 hex chars
  workspace_id    32 hex chars (the workspace living at this root)
  op_head         64 hex chars + "\n" — the only mutable pointer
  lock            lock file (OS advisory exclusive lock)
  objects/ab/cdef…  immutable objects; file name = hex id
  tmp/            temporary files; contents are never meaningful
  quarantine/     corrupt objects moved aside for diagnosis
  restore-in-progress   present only while a restore is materializing files
```

## 3. Write protocols

### 3.1 Immutable object write (`ObjectStore::write_*`)

1. If `objects/ab/<id>` exists, re-read and verify it. If valid → done
   (deduplicated). If corrupt → move it to `quarantine/` and continue.
2. Create a uniquely named file in `.arc/tmp/` with `create_new` (same
   filesystem as `objects/`).
3. Write the canonical bytes. Blobs are streamed from the source file while
   hashing; if the number of bytes read differs from the length captured when
   the header was written, the write fails with `ARC_CONCURRENT_MODIFICATION`.
4. `fsync` the temporary file.
5. Create `objects/ab/` if needed and `fsync` `objects/`.
6. Rename the temporary file to `objects/ab/<id>` (atomic on POSIX and NTFS).
7. `fsync` `objects/ab/`.

### 3.2 Pointer update (`op_head`)

Under the exclusive repository lock:

1. Read `op_head` → `expected`. Compute the new operation from the View stored
   in `expected`.
2. Write all objects (3.1), then the operation object (3.1).
3. Re-read `op_head`; if it is not `expected` → `ARC_STALE_HEAD` (cannot happen
   while the lock is honoured; checked anyway as defence in depth).
4. Write `tmp/op_head.<random>`, `fsync`, rename onto `op_head`, `fsync` `.arc/`.
   **The rename is the commit point.**

### 3.3 Init

`arc init` builds a complete `.arc` in a sibling temporary directory
`.arc-init-<random>` (format, ids, root operation object, op_head), fsyncs it,
then renames it to `.arc` and fsyncs the workspace root. A crash leaves either
no repository or a complete one; stale `.arc-init-*` directories are never
read and are reported by `arc verify`.

### 3.4 Restore (working-copy mutation)

Restore modifies user files, which cannot be made atomic as a set. Protocol:

1. Load and **fully verify** the target snapshot's tree and every blob before
   touching the working copy. Validate every path for the current platform.
2. Scan the working copy. If it differs from the workspace head and `--force`
   is absent → `ARC_DIRTY_WORKSPACE`, nothing changed.
3. With `--force` and a dirty working copy: save the working copy as a backup
   snapshot (durable, 3.1) whose parent is the head.
4. Durably write `restore-in-progress` (target id, backup id).
5. Apply: delete paths absent from the target (deepest first), then write files
   by streaming the verified blob to a temp file, `fsync`, and renaming it into
   place. Immediately before replacing or deleting a path, its current content
   hash is re-checked against the scan; a mismatch aborts with
   `ARC_CONCURRENT_MODIFICATION` (narrows, but cannot eliminate, the
   TOCTOU window against other processes).
6. Re-scan the working copy and require it to equal the target exactly.
7. Commit an operation `restore` (records target and backup) via 3.2.
8. Remove `restore-in-progress`.

If interrupted between 4 and 8, `arc status` reports the interrupted restore
and its backup id; re-running `arc restore --snapshot <target>` completes it
(the backup snapshot already preserves the pre-restore state).

## 4. Locking

Mutating commands take an OS advisory exclusive lock on `.arc/lock`
(`std::fs::File::try_lock`). The OS releases it when the process dies, so a
crash cannot leave a stale lock. A second writer fails fast with
`ARC_LOCKED`. Read-only commands do not take the lock; they read `op_head`
once and then only immutable objects, so they observe a consistent state.

## 5. Platform guarantees and limits

| Platform | File fsync | Directory fsync | Atomic rename |
|---|---|---|---|
| Linux | `fsync(2)` via `File::sync_all` | `fsync` on directory fd | `rename(2)` |
| macOS | `F_FULLFSYNC` via `sync_all` | `fsync` on directory fd | `rename(2)` |
| Windows | `FlushFileBuffers` via `sync_all` | **not available through std; skipped** | `MoveFileExW(REPLACE_EXISTING)` |

On Windows the durability of a completed rename relies on NTFS metadata
journaling; a power loss shortly after an acknowledgment could in principle
roll back the last rename. This is a **documented R1 limitation**; R6 may add
`MOVEFILE_WRITE_THROUGH`. Network filesystems and cloud-synced folders (e.g.
OneDrive, Dropbox) may not honour these primitives and are unsupported for
repositories.

## 6. Fault injection

Every step above passes through a named failpoint (`FaultPoint` enum in
`arc-objects::fs`): `TmpCreate`, `TmpWrite`, `TmpSync`, `DirCreate`,
`Rename`, `DirSync`. The op-head update uses the same primitives, so its
write/sync/rename are covered by the same points (counted per hit). Tests
configure "fail the N-th hit of point P" and also a *torn write* mode that
writes half the bytes before failing. Once a fault fires, every later
failpoint also fails (the "process" is dead), and temp files are left behind
as crash debris.

Note: a failure *after* the commit-point rename (the final directory fsync)
is reported as an error although the new state is already visible. This is
allowed by §1 ("either the previous or the new state"). The test suite iterates every point and every N reached by a
real `save`, then reopens the repository and checks the invariants of §1.
This is deterministic fault injection of the *software* failure path; it is
**not** a power-loss test and cannot detect flush-ignoring hardware.

## 7. Recovery, quarantine and retention

- On every mutating open (under the lock), files in `tmp/` are deleted.
- `arc verify --full` walks every operation reachable from `op_head`, every
  snapshot, tree and blob reachable from those operations, verifying hashes,
  kinds and graph closure. It additionally scans the whole object directory
  for files whose content does not match their name. Exit code `30`/`31` on
  corruption or missing objects. Verify never deletes anything.
- Corrupt objects found while writing are moved to `quarantine/` rather than
  deleted.
- **Retention rule (for future GC):** an object is live if it is reachable
  from any operation reachable from `op_head` (including backup snapshots).
  Because every operation retains its full View, *all* historic snapshots
  remain reachable. GC is not implemented in R1, so nothing is ever deleted.
