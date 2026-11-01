# RFC 0004 — Private workspaces, publication and local concurrency (R2)

Status: **Accepted for R2.** Builds on RFC 0001 (model and format) and RFC 0002
(durability). Implementation slices are listed in §9; undo and stored conflict
objects come in later slices.

## 1. Goals

- N private workspaces (working directories) share one object store and one
  operation log on the same machine.
- No shared mutable checkout and no shared staging index: a workspace's files
  are only ever changed by commands run in that workspace.
- Many writers (humans, agents, scripts) work at the same time. Every
  acknowledged save survives contention, retries and forced termination.
- One logical branch (`main`) advances only through an explicit,
  compare-and-swap-protected **publish**.

Non-goals for R2: line-level merging (R4), test gates and attestations (R4),
cross-machine sync (R3).

## 2. Save versus publish (behaviour change from R1)

In R1, `save` advanced both the workspace head and `main`. From R2:

| Command | Moves workspace head | Moves `main` |
|---|---|---|
| `concurv save -m` | yes, to the new snapshot | **no** |
| `concurv publish` | no | yes: CAS fast-forward to the workspace head |
| `concurv update` | yes (fast-forward or merge snapshot) | no |

Rationale: if every save also moved `main`, parallel agents would race on each
save and publish unreviewed, untested intermediate states. Separating the two
makes publication a deliberate step, and lets `update` produce a candidate
that is tested **in the private workspace** before it is published. That is
the "test the actual candidate" requirement (roadmap §3 P0 #3) without any R4
machinery.

## 3. Workspaces on disk

The **primary workspace** is the directory where `concurv init` ran. Its
`.concurv/` holds the store (objects, op log, repository lock) and its own
workspace metadata. Its name is `default`.

A **secondary workspace** created by `concurv workspace add <path>` has a
`.concurv/` directory holding **only workspace metadata**:

```
<ws>/.concurv/
  format          "concurv-workspace-format 1\n"
  store           absolute path of the primary .concurv directory, UTF-8, "\n"-terminated
  workspace_id    32 hex chars
  ws.lock         workspace lock (§5)
  tmp/            workspace-local temp files (restore materialization)
  restore-in-progress   only while a restore/checkout is materializing files
```

The primary `.concurv/` additionally has `ws.lock` for the default workspace
and `workspaces/<workspace_id>`, a locator file with the root path of each
workspace. Locator files are **not history**: losing one only means
`workspace list` cannot show that path. They live outside the op log because
absolute paths are device-local and may contain user names (RFC 0003 must
never sync them).

Opening a directory: `.concurv/format` decides. The repository format opens
the store in place. The workspace format reads `store`, requires that it
names a directory with a valid repository format, and opens the store there.

## 4. Model and format changes

`WorkspaceView` (RFC 0001 §3.4) gains a name:

```
Rid    workspace_id
String name          (NEW: unique within the view; 1–64 chars of [A-Za-z0-9._-])
u8     has_branch; if 1: String branch
u8     has_head;   if 1: Id snapshot
```

Format version 1 is not frozen before the first release (ADR 0003), so this
is a change within v1 that is recorded in the changelog. No migration is
provided because no repositories exist outside tests.

Operation kinds added: `workspace-add`, `publish`, `update`.

## 5. Locks and the commit protocol

Two kinds of locks, both OS advisory locks released by the OS on process exit:

| Lock | File | Held for |
|---|---|---|
| **workspace lock** | `<ws>/.concurv/ws.lock` | the whole command that reads or writes that workspace's files (`save`, `restore`, `update`, `workspace add` materialization) |
| **repository lock** | primary `.concurv/lock` | only the **commit step**: read op head → build new op → CAS op head |

Lock order is always workspace lock, then repository lock, so deadlock is
impossible. Both locks are acquired with a bounded wait (default 30 s,
exponential backoff from 5 ms), then fail with `CONCURV_LOCKED`.

`save` under concurrency:

1. Take the workspace lock.
2. **Without the repository lock**, scan the working copy and durably write
   blobs, trees and the snapshot object (immutable, content-addressed objects
   are safe to write concurrently; identical objects simply deduplicate).
3. Take the repository lock. Read the **current** op head and view. Require
   that this workspace's head is still the parent used in step 2 (guaranteed
   by the workspace lock; checked anyway → `CONCURV_STALE_HEAD`).
4. Build the new operation on top of the current view, changing only this
   workspace's entry. Durably write it and CAS the op head (RFC 0002 §3.2).

Other workspaces' entries are copied from the *current* view at commit time,
never from a view read earlier, so concurrent commits cannot erase each other.
The op log stays linear because commits are serialized by the repository lock.

### Temporary files under concurrency

Lock-free writers must not have their temp files deleted by another process's
recovery step. Each writing process creates a **temp session** while holding
the repository lock:

1. create and lock `tmp/s-<random>.lock`, then
2. create `tmp/s-<random>/` and write temp files only inside it.

Recovery (under the repository lock) deletes a session directory only if it
can acquire that session's lock, which proves its owner is gone. Loose files
directly in `tmp/` are only ever created under the repository lock (op-head
and marker updates), so they are deleted unconditionally there. Workspace-local
`tmp/` follows the same session rule.

## 6. Publish

`concurv publish` (in a workspace whose branch is `main`):

1. Under the repository lock, read the current `main` head `M` and workspace
   head `W`.
2. `W` absent → error (nothing saved). `M == W` → already published, no-op.
3. `M` absent, or `M` an ancestor of `W` → CAS `main` from `M` to `W` as part
   of a `publish` operation. Acknowledged when the op head CAS completes.
4. Otherwise → `CONCURV_DIVERGED` (exit 42): `main` contains work this
   workspace has not integrated. Nothing changes. The fix is `concurv update`
   followed by `publish`.

Publishing does not require a clean working copy: it publishes saved
snapshots only, never unsaved files.

## 7. Update and file-level merge

`concurv update` integrates `main` into the workspace. It requires a clean
working copy (`CONCURV_DIRTY_WORKSPACE`, exit 20, otherwise; nothing changes).

| Relationship of `M` (main) and `W` (workspace head) | Result |
|---|---|
| `M` absent, `M == W`, or `M` ancestor of `W` | up to date; nothing changes |
| `W` absent or `W` ancestor of `M` | fast-forward: check out `M`, head := `M` |
| diverged | file-level three-way merge (below) |

**Merge base.** The set of common ancestors of `W` and `M` is computed, and
every common ancestor that is itself an ancestor of another common ancestor is
removed. Exactly one remains → that is the base. None remains → the base is
the empty tree. More than one (criss-cross history) → refused with
`CONCURV_CONFLICT`, because an arbitrary choice could silently drop a change.

**File-level rule**, per path, with `b`, `o`, `t` the file state (kind and
blob, or absent) in base, ours (`W`) and theirs (`M`):

| Condition | Result |
|---|---|
| `o == t` | `o` |
| `o == b` | `t` (only theirs changed) |
| `t == b` | `o` (only ours changed) |
| otherwise | **conflict** |

The merged map must also be a valid tree: a path that is a file on one side
and a directory on the other is a conflict. Any conflict → `CONCURV_CONFLICT`
(exit 43) listing every conflicting path, and **nothing changes**. Contents
are never merged within a file in R2.

No conflict → a merge snapshot `{tree: merged, parents: [W, M]}` is written,
the working copy is checked out to it (the restore protocol of RFC 0002 §3.4),
and the workspace head is set to it in an `update` operation. Because `M` is
now an ancestor of the new head, the next `publish` is a fast-forward.

**A clean file-level merge is not a verified merge.** Two changes in different
files can still break the program together. The intended workflow is
`update` → build and test in the workspace → `publish`. R4 adds enforced gates.

## 8. Invariants (in addition to RFC 0001 §6)

- **R2-I1** A command in workspace A never modifies files of workspace B.
- **R2-I2** A commit changes only the committing workspace's view entry and,
  for `publish`, the branch it publishes. Every other view entry is copied
  from the op head current at commit time.
- **R2-I3** `main` moves only by `publish`, only forward (the old head is an
  ancestor of the new one), and only under the repository lock with a CAS.
- **R2-I4** A failed or killed save, update or publish leaves every
  previously acknowledged snapshot reachable and every view entry valid.
- **R2-I5** `update` and `publish` never discard unsaved working-copy data:
  `update` refuses a dirty working copy, and `publish` doesn't touch files.

## 9. Implementation slices and gate mapping

| Slice | Content | Status |
|---|---|---|
| 2a | workspaces (`workspace add/list`), locks with bounded wait, temp sessions, lock-free save, `publish`, `update` with file-level merge, 2/5/10-writer stress tests incl. killed processes | this RFC |
| 2b | workspace-scoped `undo` (operation-level, never moves `main` backwards) | planned |
| 2c | stored conflict objects (instead of refusing), resolution workflow | planned |
| 2d | minimum Git import/export (ADR 0002) | planned |

R2 gate (roadmap §9): 2, 5 and 10 simultaneous local writers preserve all
acknowledged changes, even during retries, forced termination and conflicting
target head updates; reopening after a crash succeeds without manual object
surgery. Slice 2a provides the tests for this gate. The stage is marked passed
only when the remaining slices also pass their tests.
