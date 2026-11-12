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
  compare-and-swap-protected **publish**. (R2.5 adds further named branches
  under the same rule; see ADR 0007.)

Non-goals for R2: line-level merging (added in R2.5, ADR 0006), test gates
and attestations (R4), cross-machine sync (R3).

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
  ws-tmp/         workspace-local temp files (restore materialization)
  restore-in-progress   only while a restore/checkout is materializing files
```

The primary `.concurv/` additionally has `ws.lock` and `ws-tmp/` for the default workspace
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
and locator updates), so they are deleted unconditionally there.

**Every temp root is cleaned only under its own lock.** The store's `tmp/` is
guarded by the repository lock; each workspace's `ws-tmp/` (session rule as
above, plus the restore marker's atomic write) is guarded by that workspace's
lock. The primary workspace's metadata lives in the same `.concurv/` as the
store, which is why the workspace directory is named `ws-tmp/` and never
`tmp/`. An earlier draft shared `tmp/` there; the 10-writer stress test showed
that the primary workspace's cleanup then deleted other writers' in-flight
temp files. On Windows that could leave `op_head` naming an object that
vanished once its "delete pending" handle closed.

Cleanup is **best effort**. Debris is never referenced, so a removal that fails
(for example a lock file that its exiting owner is deleting at the same
moment) is skipped and retried by a later cleanup, instead of failing the
user's command.

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
| otherwise | **conflict**, except: a regular text file present on all three sides whose edits do not overlap or touch is merged line by line (R2.5, ADR 0006) |

The merged map must also be a valid tree: a path that is a file on one side
and a directory on the other is a conflict. Any conflict → `CONCURV_CONFLICT`
(exit 43) listing every conflicting path, and **nothing changes**. In R2,
contents were never merged within a file. Since R2.5 non-overlapping hunks
are merged (ADR 0006).

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
- **R2-I3** `main` (since R2.5: every branch) moves only by `publish`, only
  forward (the old head is an ancestor of the new one), and only under the
  repository lock with a CAS. `branch create` only inserts a new reference.
  The branch invariants B1–B5 are in ADR 0007 §6 and §12 below.
- **R2-I4** A failed or killed save, update or publish leaves every
  previously acknowledged snapshot reachable and every view entry valid.
- **R2-I5** `update` and `publish` never discard unsaved working-copy data:
  `update` refuses a dirty working copy, and `publish` doesn't touch files.

## 9. Implementation slices and gate mapping

| Slice | Content | Status |
|---|---|---|
| 2a | workspaces (`workspace add/list`), locks with bounded wait, temp sessions, lock-free save, `publish`, `update` with file-level merge, 2/5/10-writer stress tests incl. killed processes | **implemented** |
| 2b | workspace-scoped `undo` (operation-level, never moves `main` backwards) | **implemented** (§10) |
| 2c | stored conflict objects (instead of refusing), resolution workflow | **implemented** (§11) |
| 2d | minimum Git import/export (ADR 0002) | **implemented** (ADR 0002, crate `concurv-git`) |

R2 gate (roadmap §9): 2, 5 and 10 simultaneous local writers preserve all
acknowledged changes, even during retries, forced termination and conflicting
target head updates; reopening after a crash succeeds without manual object
surgery. Slice 2a provides the tests for this gate; the stage counts as passed
only once all four slices pass their tests. That happened on 2026-11-01
(evidence in `docs/ROADMAP_STATUS.md`).

## 10. Workspace-scoped undo (slice 2b)

`concurv undo` reverses the most recent operation **of the current
workspace** that is not itself an undo and has not been undone yet, and it
reverses **exactly what that operation changed**:

| Undone operation | What it changed | What undo does | Working-copy precondition |
|---|---|---|---|
| `save` | workspace head | head := head before the save; files untouched (the saved content shows up as an unsaved change again) | none (files are not touched) |
| `update` (fast-forward or merge) | files and head | files and head := the head before the update | working copy equals the current head |
| `restore` | files only | files := the backup snapshot if the restore was forced, else the head | working copy equals the restored snapshot |
| `publish` | the shared branch | **refused** (invariant I8: others may build on it; make a new reverting change instead) | — |
| `workspace-add`, `init` | registry / repository | **refused** | — |

Rules:

- **Scope.** Undo writes only the current workspace's view entry and working
  files. It never changes another workspace's entry or any branch, so it
  cannot revert shared history (I8, R2-I1, R2-I2).
- **Multi-level.** Repeated undo walks further back. The undo operation records
  the operation it reversed (`undoes`), and later undos skip both. Undo is not
  itself undoable: to go forward again, `restore` the snapshot shown by
  `op log` and save.
- **State before.** Only the workspace's own operations change its entry, so
  the entry "before" operation O is the entry in O's parent operation's view.
  Undo also requires that the workspace head still equals the entry O left.
  This is guaranteed by the rules above and checked anyway (`CONCURV_STALE_HEAD`).
- **Safety.** Any file change goes through the restore protocol (RFC 0002
  §3.4) with the precondition above. If the working copy holds anything else,
  undo refuses with `CONCURV_DIRTY_WORKSPACE` and nothing changes. No undone
  snapshot is ever deleted: every one stays reachable from the op log.
- **The first checkout of a new workspace** (the `update` recorded by
  `workspace add`) cannot be undone: there is no earlier head to return to.

Operation format additions (RFC 0001 §3.4, pre-release within v1):
`undoes: Option<OperationId>` (set on `undo` operations) and
`checked_out: Option<SnapshotId>` (the snapshot an operation made the working
copy equal to; set by restore, update, workspace-add checkout and file-changing
undos). The latter is what lets undo check the precondition for `restore`
without parsing descriptions.

## 11. Recorded conflicts and resolution (slice 2c)

`concurv update` still **refuses** conflicts by default (exit 43, nothing
modified), so scripts written against slice 2a keep working.
`concurv update --allow-conflicts` instead starts a **pending merge**:

1. The clean part of the file-level merge (§7) is applied as usual.
2. Each conflicted path `P` is written for resolution:

   | Conflict | `P` in the working copy | Side files |
   |---|---|---|
   | both sides changed a text file (UTF-8, no NUL, ≤ 8 MiB on all sides) | whole-file markers: `<<<<<<< ours …`, ours, `\|\|\|\|\|\|\| base …`, base, `=======`, theirs, `>>>>>>> theirs …` | `P.concurv-conflict` |
   | binary / symlink / too large, both changed or both added | ours, unchanged | `P.concurv-conflict`, `P.concurv-theirs` (theirs' bytes) |
   | deleted on one side, changed on the other | ours (absent if ours deleted it) | `P.concurv-conflict`, plus `P.concurv-theirs` if theirs has it |
   | file on one side, directory on the other | — | **still refused**, nothing modified |

   `P.concurv-conflict` is a plain-text explanation: what each side did and
   how to resolve. A side-file name that is already in use refuses the merge.
3. The resulting working tree is stored as a snapshot (the *worktree*), and an
   immutable **conflict object** (object kind 5) records `ours` (the workspace
   head), `theirs` (the branch head), the merge base, the worktree snapshot,
   and per path the conflict kind, whether markers were written, and the side
   files. The workspace view entry gains `merge: Some(conflict id)`. The
   workspace head does **not** move.
4. The working copy is checked out to the worktree snapshot (RFC 0002 §3.4),
   and the `update` operation records it as `checked_out`.

A path is **resolved** when none of its side files exists and, if markers were
written, no line of `P` starts with `<<<<<<< ours`, `||||||| base` or
`>>>>>>> theirs`. (`=======` alone is not treated as a marker because it is
common in ordinary text.)

While a merge is pending:

| Command | Behaviour |
|---|---|
| `status` | reports the merge and each conflicted path as resolved / unresolved (with what remains) |
| `save` | refused with `CONCURV_CONFLICT` while anything is unresolved. Once resolved, records a snapshot with parents `[ours, theirs]`, **even if the content equals ours**, clears `merge`, and moves the head. The next `publish` is a fast-forward |
| `update` | refused (a merge is already in progress) |
| `update --abort` | checks out `ours`, clears `merge` (operation kind `merge-abort`). If the working copy differs from the worktree snapshot (partial edits), a backup snapshot is taken first, as with `restore --force` |
| `undo` | right after the conflicted update, with files unchanged: returns to `ours` and clears `merge`. After the merge-completing save: the head returns to `ours` and the merge is pending again, with the already-resolved files. `merge-abort` cannot be undone |

Limits: markers cover the **whole file**. Hunk-level, line-by-line merging is
R4. Side files and markers never reach a snapshot, because `save` refuses
until they are gone.

## 12. Branches (R2.5, ADR 0007)

The view's `branches` map holds any number of named references. Every
workspace's `branch` field names the one it tracks; `publish` and `update`
use it. New operation kinds: `branch-create` (inserts a reference, and with
`--switch` also sets the creating workspace's `branch`), `branch-switch`
(checkout plus a change of the tracked branch in one operation) and `merge`
(the integration of §7 with another branch as the source).

**Kinds of state.** These terms are used consistently from here on:

- *Workspace state*: files, workspace head, pending merge. Private to one
  workspace.
- *Local branch reference*: `view.branches[b]`, shared by all workspaces of
  the repository. Before R3, this is also the authoritative published head.
- *Remote-tracking reference* (R3): the last relay head observed by fetch,
  stored apart from `branches` (`View::remotes`, RFC 0001 §3.4; RFC 0003 §5).
- *Authoritative published head* (R3): the relay's head of a branch, moved
  only by its CAS.

**Switch protocol.** `branch switch` uses the checkout of RFC 0002 §3.4 in
transactional mode:
1. Refuse dirty tracked files, colliding untracked or ignored files, a
   pending merge, and a head that would be on no branch (ADR 0007 §3).
2. Write the marker with `origin` (the old head) and `branch`.
3. Apply. Non-colliding untracked files are part of the expected result, so
   they are never touched.
4. Verify by rescanning, then commit `{head := target, branch := b}`.

A failure before the commit point rolls the files back. Content the switch
did not write is never touched by the rollback. After a crash, repeating the
switch resumes at the marker's target. In every checkout, a path whose
current content equals its target content is not unsaved work.
