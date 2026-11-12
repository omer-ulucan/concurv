# ADR 0007 — Branch model: shared references, private workspaces

Status: **Approved in principle by the owner (2026-11-01); refinements
implemented and verified** (CI run 41375647699 on `main` @ `79caf3f`; see
`docs/ROADMAP_STATUS.md`, R2.5).
Final acceptance is the owner's decision. Sections 7 and 8 (R3) are design
only.

## Context

R2 had one hard-coded branch, `main`. The format already allowed more: the
operation view maps branch names to snapshots, and each workspace view
records the branch it tracks (RFC 0001 §3.4, RFC 0004 §4). A first slice
added `branch create/list/switch`. The owner approved the model in principle
and asked for refinements: complete Git-compatible naming, preserving
unpublished work, a separate `merge` command, a transactional switch, R3
reconciliation rules, more tests, and documentation of the reference kinds.

## Decision

1. **Core model (unchanged).** A **branch** is a shared, named reference to
   a snapshot. It is the unit of integration and moves only forward, by
   compare-and-swap (`publish`). A **workspace** is the unit of parallel
   work: private files, head and pending merge. Every workspace tracks
   exactly one branch, its `publish` and `update` target. Any number of
   workspaces may track one branch. There is no detached state and no
   shared index.

2. **Commands.**

   | Command | Effect |
   |---|---|
   | `branch create <name> [--at <snapshot\|branch>]` | add a reference. The default start is this workspace's head, **including saves not yet published**, so nothing is left behind. Moves nothing else |
   | `branch create <name> --switch` | the same at this workspace's head, and the workspace tracks it, **in one operation**. No files change. The way to keep unpublished saves on their own branch |
   | `branch list` | references, heads, and the workspaces that track them |
   | `branch switch <name>` | track another branch and check out its head, transactionally (3) |
   | `workspace add <dir> --branch <name>` | a new workspace tracking `name` |
   | `update` | integrate **the tracked branch** (fast-forward or merge) |
   | `merge <branch>` | integrate **another** branch with the same rules (clean working copy, exact merge base, line-level merge, conflicts refused or recorded). The workspace keeps its branch. The result becomes a published head only through a later `publish` (CAS). Merging the tracked branch is refused (use `update`) |
   | `git export --from <branch> [--branch <git>]` | export any branch; snapshots already exported are reused |

   Compatibility: `update` and its flags are unchanged. The `update --from`
   option of the first slice never shipped in a release; `merge` replaces it.

3. **Switching is transactional.** `branch switch` changes nothing and fails
   if:
   - a tracked file has unsaved changes (exit 20);
   - an **untracked** file would be overwritten or removed: a different file
     at a target path, a file where the target needs a directory, or a file
     inside a path the target makes a file (exit 20);
   - an **ignored** file would be overwritten (exit 20): concurv cannot back
     it up;
   - a merge is pending (exit 43);
   - the branch does not exist (exit 50);
   - the workspace head contains saves that neither the old nor the new
     branch contains (exit 42), because they would end up on no branch.
     Remedy: `branch create <name> --switch`, or `publish`.

   Untracked files that do not collide stay where they are and remain
   untracked. Writing goes through the restore protocol (RFC 0002 §3.4) with
   a marker recording the **origin** head and the target branch. If writing
   or committing fails in a running process, the working copy is **rolled
   back** to its original state. Only content the switch itself wrote is
   replaced or removed; anything else found on disk is kept. If the process
   dies, every file holds its old or its new content, and running the same
   `branch switch` again finishes it (it resumes at the recorded target).
   `restore --snapshot <origin> --force` goes back instead. This rests on a
   rule now applied to every checkout: a path whose current content already
   equals the target is not unsaved work, because nothing is lost by leaving
   it.

4. **Undo.** `undo` reverses a `branch-switch` (files, head and tracked
   branch) and a `merge`, like an `update`. It refuses `branch-create`
   (including `--switch`), like `publish`, because branches are shared and
   someone may already build on the new one.

5. **Names.** Three layers, all checked when a branch is created, under the
   repository lock (so concurrent creations have one winner):
   1. **Git's rules** for `refs/heads/<name>`, exactly, verified against
      `git check-ref-format` by a differential test: no `..`, `@{`, control
      characters, space, `~ ^ : ? * [ \`; no leading, trailing or double `/`;
      no part starting with `.` or ending with `.lock`; no trailing `.`.
   2. **Portability policy:**
      - ASCII only (non-ASCII names can alias each other under Unicode
        normalization when Git stores them as files);
      - no `< > " |` and no Windows device names (`CON`, `NUL`, `COM1`, …)
        as a part, because Git for Windows stores branches as files;
      - at most 200 bytes;
      - no leading `-`, no `HEAD` or `@`;
      - first parts `refs`, `remote` and `remotes` are reserved
        (remote-tracking names, 8).
   3. **Namespace:** no collision with an existing branch. Two names collide
      if they are equal, if one is a `/`-prefix of the other (Git cannot
      hold `x` and `x/y`), or if they share a leading part that differs only
      in case (`Topic/y` vs `topic/x` alias on case-insensitive
      filesystems).

   Names already present in a stored view are only checked by the format
   rule (RFC 0001), so existing repositories stay readable.

6. **Invariants.**
   - **B1 Reachability:** no branch command makes a snapshot that was a
     workspace head or branch head unreachable from the current workspace
     heads and branch heads.
   - **B2 Forward-only:** a branch reference is created at a complete,
     verified snapshot and afterwards moves only by a fast-forward
     compare-and-swap (`publish`, and since R3 the fast-forward of `fetch`
     to the relay's head). No branch command moves it.
   - **B3 Transactional switch:** a failed switch leaves files, head, tracked
     branch and marker as they were (rollback). An interrupted one leaves
     only old-or-new file content and is finished by repeating it.
   - **B4 Namespace:** no two branches collide (5.3).
   - **B5 No implicit publication:** `merge`, `update`, `switch` and
     `create` never move an existing branch; only `publish` does (and
     `fetch`, only forward to what the relay published, R3).

7. **Sync (R3, implemented).** Branch reconciliation is per branch and
   never loses history (RFC 0003 §5). A fetch records the relay's head as a
   remote-tracking reference. The local reference may only fast-forward.
   Divergence is preserved and must be integrated by `merge` in a
   workspace (`concurv merge remote/<remote>/<branch>`), pass conflict
   checks (and, from R4, gates), and then be published by a compare-and-swap
   that expects the observed remote head. A merge candidate never becomes a
   published head automatically. A relay head that moved backwards or
   sideways is reported, not followed. The rules are encoded in
   `concurv_workspace::reconcile::plan`, used by `concurv fetch` and
   `concurv push`, and tested with an in-memory model
   (`tests/reconcile.rs`) and against a real relay (`concurv-sync` tests).

8. **Kinds of state** (the vocabulary used everywhere):

   | Kind | Where | Moved by | Shared? |
   |---|---|---|---|
   | Workspace state: files, workspace head, pending merge | the workspace directory and its view entry | `save`, `update`, `merge`, `restore`, `undo`, `branch switch` in that workspace | never (private; not synced, RFC 0003 §5) |
   | Local branch reference `view.branches[b]` | this repository's operation view | `branch create` (insert), `publish` (CAS fast-forward), `fetch` (fast-forward only, or insert of a new name) | by every workspace of this repository |
   | Remote-tracking reference `remote/<remote>/<b>` (R3) | a separate map in the view (`View::remotes`, repository format 2), never in `branches` | only `fetch` and `push` | read-only locally; `merge` can integrate it |
   | Authoritative published head | the relay (RFC 0003 §1) | only the relay's CAS `Publish` | the source of truth across machines |

   Before R3 there is no relay. Each repository's local branch references
   are the authoritative published heads for all of its workspaces.

9. **Future (not implemented, roadmap):** deleting and renaming branches
   (policy for workspaces that track them, and for sync), protected branches
   (R4 gates) and tags (R6, with the Git tag mapping). Remote-tracking
   references were added by R3 (item 7).

## Consequences

- No repository-format change. New operation kinds: `branch-create`,
  `branch-switch`, `merge`. The restore marker gains optional `origin` and
  `branch` lines (older markers still parse).
- The relaxed dirty rule (a path already at its target content is not
  unsaved work) also applies to `restore`, `update` and checkout. It can only
  allow operations that lose nothing.
- Fault injection gains a transient mode (`Faults::fail_once_at`) next to
  the crash mode, to test rollback in a process that keeps running.

## Verification

| Requirement | Tests |
|---|---|
| Git-compatible names | `refname.rs` unit tests; `tests/refname_vs_git.rs` (≈460 names vs `git check-ref-format`; every accepted name is a valid Git branch) |
| Namespace collisions, concurrent creation | `branches.rs::create_refusals_change_nothing`, `concurrent_creates_of_one_name_have_one_winner`, `concurrent_creates_of_colliding_names_have_one_winner` |
| Unpublished saves preserved (B1) | `create_switch_keeps_unpublished_saves_on_the_new_branch`, `switch_never_loses_work`, reachability checks in several tests |
| Several workspaces on one branch; CAS failures | `several_workspaces_follow_one_branch_without_losing_saves`, `stale_publish_to_a_branch_is_a_cas_failure_that_changes_nothing`, `publication_is_isolated_per_branch`, `concurrent_publishes_to_different_branches_all_land` |
| `merge` vs `update` | `merge_another_branch_then_publish_to_own` (incl. undo); CLI `branch_commands` |
| Transactional switch: dirty, untracked, ignored, pending merge | `switch_never_loses_work`, `switch_keeps_untracked_files_and_refuses_collisions`, `switch_never_overwrites_an_ignored_file`, `switch_is_refused_during_a_pending_merge` |
| Filesystem errors and crashes (B3) | `tests/branch_switch_faults.rs`: every fault point × every hit, transient (rolled back to the exact original state) and crash (every file old-or-new, untracked file intact, resumed by repeating the switch); `interrupted_switch_can_be_finished_or_undone`; CLI `kill_recovery.rs::killed_branch_switches_are_finished_by_running_them_again` (real processes killed mid-switch) |
| Future cross-machine divergence | `tests/reconcile.rs` (divergence preserved and never auto-published, one CAS winner, rewound relay not followed) |
