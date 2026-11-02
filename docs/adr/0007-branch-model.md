# ADR 0007 — Branch model: named branches and workspaces

Status: **Accepted, implemented** (R2.5, P1-2/P1-3). Date: 2026-11-01. The
owner delegated the decision ("finish R2 completely") and may revise it
before 0.1.0.

## Context

R2 had one hard-coded branch, `main`. The format already allowed more: the
operation view maps branch names to snapshots, and each workspace view
records the branch it tracks (RFC 0001 §3.4, RFC 0004 §4). Missing were the
commands, the rules for moving between branches, and how branches relate
to workspaces, to Git and to future sync (R3).

## Decision

1. **Two concepts, two jobs.** A **branch** is a shared, named pointer to a
   snapshot. It is the unit of integration: others build on it, it moves
   only forward, and only by compare-and-swap (`publish`). A **workspace**
   is the unit of parallel work: a private working copy and head. Every
   workspace tracks exactly one branch, which is its `publish` and `update`
   target. Several workspaces may track one branch. There is no detached
   state and no shared index.
2. **Commands.**
   - `branch create <name> [--at <snapshot|branch>]` adds a branch at this
     workspace's head by default (or, without a head, at its branch's head).
     It moves no workspace. Operation kind `branch-create`.
   - `branch list` shows every branch, its head, and the workspaces that
     track it.
   - `branch switch <name>` makes the current workspace track `name` and
     checks out its head (the restore protocol, RFC 0002 §3.4). Operation
     kind `branch-switch`.
   - `workspace add <dir> --branch <name>` creates a workspace on another
     branch. The default is the current workspace's branch.
   - `update --from <branch>` merges another branch into this workspace with
     the same rules as `update`. The workspace keeps its branch, so the
     next `publish` moves that branch. This is how work moves between
     branches, e.g. `feature` into `main`.
   - `git export --from <branch> [--branch <git-branch>]` exports any branch.
     Snapshots already exported are reused, so branches share Git history.
3. **Switching never loses work.** `branch switch` is refused, with nothing
   changed, if:
   - the working copy has unsaved changes (`CONCURV_DIRTY_WORKSPACE`), or an
     ignored file is in the way;
   - a merge is pending (`CONCURV_CONFLICT`);
   - the branch does not exist (`CONCURV_NOT_FOUND`);
   - the workspace head contains saves that neither the old nor the new
     branch contains (`CONCURV_DIVERGED`), because they would end up on no
     branch. Remedies: `publish`, or `branch create <new>` and switch to it.
4. **Undo.** `undo` reverses a `branch-switch`: it restores the files, the
   head and the tracked branch. It refuses `branch-create`, like `publish`,
   because branches are shared and others may already use the new one.
5. **Names.** New branch names follow a subset of Git's ref-name rules, so
   every branch exports under the same name: ASCII letters, digits and
   `._-/`, at most 200 bytes, no `..`, `//`, `.lock` suffix, `HEAD`, leading
   `-`, or empty `/` part, and no part that starts or ends with `.`. Names
   already in a view are only checked by the format rule.
6. **Not in this slice.** Deleting and renaming branches (they need a policy
   for workspaces that track them and for sync), protected branches (R4),
   and tags (R6, with the Git tag mapping).
7. **Sync (R3).** Branches travel in the operation view like everything
   else. Reconciling two devices' views means reconciling each branch
   separately: a fast-forward is taken, and a divergence is integrated with
   `update --from`, never by moving a branch sideways. RFC 0003 must be
   revised accordingly before R3.

## Consequences

- No format change: the view already stores branches. New operation kinds:
  `branch-create`, `branch-switch`.
- Concurrency is unchanged: branch creation and publication are
  compare-and-swaps under the repository lock, so a name has one winner,
  and publications to different branches never interfere.

## Verification

- `crates/concurv-workspace/tests/branches.rs` (12 tests): create/list,
  refusals, creating at an older snapshot, per-branch publication and CAS
  isolation, `update --from` then publish, switch and publish target,
  switch-never-loses-work (dirty, unpublished saves, the `branch create`
  remedy), pending-merge refusal, undo of a switch and refusal for a create,
  `workspace add --branch` on a missing branch creating nothing, 5
  concurrent creates of one name (one winner), and 4 concurrent publishes
  to different branches.
- `branches.rs` unit test of the name rules.
- `concurv-git/tests/bridge.rs::export_of_a_feature_branch_shares_history_with_main`.
- CLI: `cli_smoke.rs::branch_commands`.
