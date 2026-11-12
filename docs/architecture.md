# concurv architecture (as implemented: R1 to R3)

```
concurv-cli          binaries `concurv` and `concurvd`; clap, human and JSON output
 ├─► concurv-git     Git bridge (fast-import / fast-export)
 └─► concurv-sync    wire protocol, relay, pinned TLS, fetch / push / clone, remotes
      └─► concurv-workspace  repo, workspaces, scan, save, status, diff, restore,
           │                 publish, update, merge, branches, reconcile, graph, verify,
           │                 policy and check (R4: protected branches, attestations)
           └─► concurv-ops   OpLog: commit, walk, clean_tmp
                └─► concurv-refs     op_head CAS, repository lock
                     └─► concurv-objects  encoding, store, durable fs, faults,
                          │                locks, temp sessions
                          └─► concurv-core ids, errors, path rules
```

| Crate | Key types | Responsibility |
|---|---|---|
| `concurv-core` | `ObjectId`, `BlobId`/`TreeId`/`SnapshotId`/`OperationId`, `ChangeId`/`WorkspaceId`/`DeviceId`, `ConcurvError`, `ErrorCode`, `RepoPath`, `PlatformRules` | domain ids with compile-time separation; stable error/exit codes; name validation and Windows/case-folding rules |
| `concurv-objects` | `encoding::{Tree, Snapshot, Operation, View, WorkspaceView}`, `ObjectStore`, `fs::{TempFile, TempSession, FileLock, Faults}` | RFC 0001 byte format with strict decoders; verified loose-object store; durable write primitives with deterministic failpoints; bounded-wait OS locks; per-process temp sessions and best-effort debris cleanup |
| `concurv-refs` | `OpHead`, `RepoLock` | the only mutable pointer and its CAS; the repository (commit) lock |
| `concurv-ops` | `OpLog` | commit an operation (re-verify referenced snapshots, write op, CAS); walk history; clean store temp debris |
| `concurv-workspace` | `Repo`, `WorkspaceLock`, `StoreWriter`, `FlatTree`, `IgnoreRules`, modules `save`, `status`, `restore`, `integrate` (publish/update), `conflicts` (pending merges), `undo`, `merge`, `graph`, `workspaces`, `ignore` (ADR 0004), `verify` | everything touching working copies, workspaces and branch publication |
| `concurv-git` | `GitRepo`, `export`, `import`, `GitMap` | Git bridge (ADR 0002): drives `git fast-import` / `fast-export`; deterministic commits; snapshot↔commit cache |
| `concurv-sync` | `Relay`, `Server`, `Connection`, `RelayUrl`, modules `proto`, `tls`, `graph`, `remote`, `sync` (`fetch`, `push`, `clone_repo`) | RFC 0003: framing and messages, pinned TLS 1.3, relay storage and publish CAS, quarantine and graph closure, client operations and remote configuration |
| `concurv-cli` | `concurv` and `concurvd` binaries | argument parsing, human and JSON output |

## Key decisions

- **One mutable pointer.** Every operation object stores the complete View
  (branches + every workspace's head). A mutation = write immutable objects →
  write operation → atomic rename of `op_head`. See ADR 0001, RFC 0002.
- **Save is private, publish is shared** (RFC 0004 §2). `save` moves only the
  workspace head; `publish` fast-forwards `main` by CAS; `update` integrates
  `main` into the workspace, so merged results are tested before they are
  published.
- **Two lock levels.** A workspace lock serializes commands on one working
  copy; the repository lock is held only for the commit step. Content
  objects are written lock-free in per-process temp sessions. Every temp root
  is cleaned only under its own lock (store `tmp/`: repository lock; `ws-tmp/`:
  workspace lock). See RFC 0004 §5 for the bug that made this rule explicit.
- **Commits apply to the current view.** A commit copies every other
  workspace's entry from the op head current at commit time, so concurrent
  commits cannot erase each other, and the op log stays linear.
- **Branches are shared pointers, workspaces are private work.** Every
  workspace tracks one branch; branch creation and publication are CAS
  operations under the repository lock; `merge <branch>` moves work between
  branches, and only `publish` moves a branch. `branch switch` is a
  transactional checkout (rollback on failure, resumable after a crash),
  and in every checkout a path already at its target content is not
  unsaved work (ADR 0007).
- **Conservative merge, exact merge base.** Files merge at file level;
  text files changed on both sides merge line by line only if no hunks
  overlap or touch (ADR 0006). Everything else is a conflict, and ambiguous
  (criss-cross) bases are refused, never guessed.
- **Content is the oracle.** `save`, `restore`, `update`, `undo` and checkout
  hash every file. `status`/`diff` may skip an unchanged file only on Unix,
  through a disposable stat cache that only `save` writes. An entry must match
  device, inode, size, mode, mtime and the kernel-maintained ctime exactly,
  and is recorded only when its timestamps are outside a 2 s racy window
  (ADR 0005). Windows always hashes. `save` hashes first and writes (and
  fsyncs) only objects that are missing or fail verification.
- **Restore never rewrites history** and never discards unsaved data without
  `--force` + durable backup.
- **One authority per published branch.** Across machines, a relay holds
  the published heads and moves them only by compare-and-swap, only
  forward. Clients keep what they saw in remote-tracking references (a
  separate map in the view) and never follow a head that went backwards.
  Everything received is quarantined, verified and checked for graph
  closure before anything references it (RFC 0003).
- **Evidence before publication (R4).** A branch protected by the policy in
  its current head moves only to a candidate with a passing attestation
  bound to that exact snapshot, branch and policy version; checks run on a
  fresh checkout of the snapshot, only for policies approved on the
  device. The relay applies the same rule (RFC 0005).
- **Synchronous engine.** No async runtime; concurrency comes from processes
  and threads coordinating through OS locks.

## Data flow of `concurv save` (R2)

1. Take this workspace's lock; read the workspace head and its file map.
2. Briefly take the repository lock to open a temp session (and clean dead
   sessions); release it.
3. Walk the working copy (no symlink following); stream each file into the
   session, fsync, rename into `objects/`, fsync dir (dedup verifies existing).
   Build canonical trees bottom-up; write the snapshot. Other workspaces may
   commit during this step.
4. Take the repository lock; read the **current** op head; check the
   workspace head is unchanged; set it to the new snapshot in a copy of the
   current view; write the operation; CAS `op_head`.

## Not yet implemented

Importing Git changes into existing history, GC/packing, rename detection,
`workspace remove`, branch delete/rename, several repositories per relay,
sandboxed checks and agent coordination (R5). See
`ROADMAP_STATUS.md`.
