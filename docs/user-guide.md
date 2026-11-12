# concurv user guide

This guide covers what the current build actually does: several private
workspaces sharing named branches (`main` first) on one machine, sync
between machines through a relay you run (§8a), protected branches (§8b),
coordination between agents (§8c), tags (§4b) and the Git bridge (§9).
Stage status and evidence: `docs/ROADMAP_STATUS.md`.

## 1. Install

**Prebuilt binaries:** download the archive for your platform from
<https://github.com/omer-ulucan/concurv/releases>, verify it with
`sha256sum -c SHA256SUMS --ignore-missing`, unpack it, and put `concurv` and
`concurvd` (`.exe` on Windows) on your `PATH`. v0.1.0-alpha.3 ships Linux
x86_64 (static, any distribution) and Windows x86_64 (as did alpha.2); v0.1.0-alpha.1 also had
macOS builds. The binaries are not code-signed; on macOS, run
`xattr -d com.apple.quarantine concurv` once if Gatekeeper blocks it.

**From source** (Rust 1.89 or newer; installs `concurv` and `concurvd`):

```console
$ cargo install --git https://github.com/omer-ulucan/concurv --tag v0.1.0-alpha.3 concurv-cli
```

Or from a clone:

```console
$ git clone https://github.com/omer-ulucan/concurv.git
$ cd concurv
$ cargo install --path crates/concurv-cli    # installs `concurv` and `concurvd` into ~/.cargo/bin
$ concurv --version
concurv 0.1.0-alpha.3
```

Or build without installing: `cargo build --release` puts the binaries at
`target/release/concurv` and `target/release/concurvd` (`.exe` on Windows).

**Where not to put repositories:** do not create repositories inside
cloud-synced folders (OneDrive, Dropbox, iCloud Drive) or on network
filesystems. Their rename and flush behaviour is outside the durability
guarantee.

## 2. Concepts in one minute

- **Working copy:** the files in your project directory. concurv never records
  them until you run `concurv save`.
- **Snapshot:** an immutable, content-addressed record of every file at one
  moment, plus its parent, author, time and message. It's identified by a
  64-character hex id; any unique prefix of at least 8 characters works as input.
- **Workspace:** a directory with its own working copy and its own head. The
  directory where you ran `init` is workspace `default`; `workspace add`
  creates more. A command only ever changes the files of the workspace it runs in.
- **Head:** the snapshot this workspace's working copy is based on. `save`
  advances it. It is private to the workspace.
- **`main`:** the shared branch. It moves only when you run `publish` (or when
  `fetch` brings it forward from a relay), and only
  forward.
- **Operation log:** every operation of every workspace is recorded separately
  from snapshot history, including backups made by `restore --force`.

All metadata lives in `.concurv/` at the project root. Never edit it by hand.

## 3. Everyday workflow

```console
$ concurv init                       # once, in the project root
$ concurv status                     # what changed since the head?
$ concurv diff                       # line-level diff of text files
$ concurv save -m "Add parser"       # record a snapshot in this workspace
$ concurv publish                    # make it the new `main`
$ concurv log                        # history, newest first (-n 10 to limit)
$ concurv op log                     # every operation, including backups
$ concurv undo                       # reverse your last save/update/restore
```

Details that matter:

- **Ignoring files works like Git.** Put patterns in `.gitignore` files (any
  directory; deeper files win, `!` re-includes) or in the local, never-shared
  `.concurv/exclude`. `concurv check-ignore <path>` tells you why a path is or
  isn't ignored. `.concurv/` and `.git/` are never versioned, and neither are
  nested repositories. Details: section 3a below.
- Empty directories are not versioned, the same as Git.
- A rename shows up as `deleted` plus `added`.
- `status` compares file **contents**, not timestamps, so it never misses an
  edit. The cost is that it reads every file on each run.
- `save` with no changes prints `Nothing to save` and records nothing.
- Author: set `CONCURV_AUTHOR="Name <email>"`. Otherwise concurv uses the OS
  user name, falling back to `unknown`.
- `diff` shows text diffs for UTF-8 files up to 8 MiB. Binary, larger and
  symlink changes get a one-line note instead.
- `save` records your work durably **in your workspace only**. Nobody else
  sees it until you `publish`.

## 3a. Ignoring files

concurv reads the same ignore files Git does:

```gitignore
# .gitignore at the project root
/target
node_modules/
.env
*.log
!keep.log
```

- **Sources**, from strongest to weakest: the `.gitignore` in the deepest
  directory, then shallower ones, then the local `.concurv/exclude` (yours
  only; never saved or shared, like `.git/info/exclude`).
- **Rules** are Git's: `#` comments, `\#` and `\!` escapes, trailing spaces
  ignored, `!` re-include, `/` anchors to the file's directory, a trailing
  `/` matches directories only, `*`, `?`, `[abc]`, `**`. A file inside an
  excluded directory cannot be re-included, exactly as in Git.
- **Already-tracked files stay tracked.** Adding a rule that matches a file
  you've already saved doesn't drop it: changes still show up and are saved,
  and deleting it still shows as `deleted`. Rules only hide **untracked**
  files.
- **Ignored files are never touched.** `restore`, `update`, `undo` and
  workspace checkouts refuse (exit 20, nothing modified) rather than
  overwrite an ignored file, even with `--force`, because a backup can't
  contain it. Move it away first.
- `.concurv/` and `.git/` are never versioned, and a subdirectory that is its
  own Git or concurv repository is skipped.
- Matching is **case-sensitive on every platform** (Git on Windows and macOS
  defaults to case-insensitive).
- Concurv agrees with Git on everything its differential test covers
  (`crates/concurv-workspace/tests/ignore_vs_git.rs`). One known difference:
  for a reversed range like `[z-a]`, Git matches the first character; concurv
  matches nothing.
- Ignore rules **don't protect secrets** that were already saved; they only
  keep new untracked files out.

Ask why a path is or isn't ignored:

```console
$ concurv check-ignore target/debug/app src/main.rs build/
target/debug/app: ignored; parent `target/` excluded by `/target` in .gitignore
src/main.rs: not ignored; no rule matches
build/: not ignored; no rule matches
```

A pattern concurv cannot use (for example an unclosed `[`) never matches, as in
Git, and `check-ignore` lists it under a warning.

## 4. Working in parallel: workspaces, update and publish

Give every person, agent or task its own workspace. Never let two of them share
one directory.

```console
$ cd ~/code/project                      # the workspace created by `init`
$ concurv workspace add ../project-agent-a
$ concurv workspace add ../project-agent-b --name reviewer
$ concurv workspace list
```

A new workspace must be a new or empty directory, and must not be inside
another workspace (or contain one). It starts with a checkout of `main`.

When a task is done, unregister its workspace from another one:

```console
$ concurv workspace remove agent-a        # refused while it has unsaved or unpublished work
```

Nothing on disk is deleted: the directory keeps its files (they are plain
files afterwards) until you delete it. Removal is refused for the primary
workspace and the one you run it in; with unsaved changes or a pending
merge there (exit 20 or 43); and with saves that no branch, tag or other
workspace contains (exit 42: publish them, or `concurv tag create <tag>
--at <head>`). If you already deleted the directory, removal still works
as long as no saves would be lost.

The cycle in each workspace:

```console
$ concurv save -m "Implement token refresh"   # private, durable
$ concurv update                              # bring in what others published
$ cargo test                                  # test the combined result here
$ concurv publish                             # main := your head (compare-and-swap)
```

What `update` does:

| Situation | Result |
|---|---|
| `main` has nothing new | `up_to_date`, nothing changes |
| you have no unpublished work | fast-forward: your files and head move to `main` |
| both sides have new work | **three-way merge**: changes to *different files* merge, and so do edits to *different parts of the same text file* (separated by at least one unchanged line; listed as "merged line by line"). A merge snapshot is created and checked out. Run your tests before publishing |
| both sides changed the **same lines** of a file (or touching lines, or a binary file, or one deleted what the other changed) | refused with exit 43 `CONCURV_CONFLICT`, listing the paths; **nothing is modified**. Use `update --allow-conflicts` to resolve them (below) |
| a file on one side is a directory on the other | always refused, nothing modified |

`update` needs a clean working copy, so `save` first. It never publishes.

What `publish` does: if `main` is an ancestor of your head, `main` moves to
your head atomically. If someone else published first, it is refused with
exit 42 `CONCURV_DIVERGED` and nothing changes. Then run `update`, test, and
`publish` again. Agents can simply loop `update` → test → `publish` until
publish succeeds.

**A clean merge is not proof the code works.** Two edits to
different files, or to different parts of one file, can still break the build together. That is why the merge
happens in your workspace, where you can test it, and not in `main`.

### Resolving conflicts

```console
$ concurv update --allow-conflicts      # writes the conflicts into your files
$ concurv status                         # lists each conflict: resolved or not
  ... edit files, delete the side files ...
$ concurv save -m "Merge main"           # completes the merge (both parents)
$ concurv publish
```

With `--allow-conflicts`, every change that merges cleanly is applied, and
each conflicted file `P` is written for you to resolve:

- **Text files** changed on both sides contain conflict markers around the
  *whole file*:
  ```
  <<<<<<< ours (workspace b, snapshot 1a2b…)
  your version
  ||||||| base (snapshot 9f8e…)
  the common ancestor
  =======
  their version
  >>>>>>> theirs (main, snapshot 4c5d…)
  ```
  Edit `P` to the right result and remove the marker lines.
- **Binary files, symlinks, and delete/modify conflicts** keep your version at
  `P` (or no file, if you deleted it). Their version is in
  `P.concurv-theirs`. Keep, replace or delete `P` as needed.
- Every conflicted path also gets **`P.concurv-conflict`**, a short note
  explaining what each side did. **Delete it once `P` is resolved**, together
  with any `P.concurv-theirs`.

`save` refuses (exit 43) until every side file is gone and no marker lines
remain, so a half-resolved merge can never be recorded or published. Your
workspace head does not move until that save.

Changed your mind? `concurv update --abort` puts your files back to your head
and ends the merge. If you had already edited files, they are kept in a backup
snapshot first. Right after `update --allow-conflicts` (before editing),
`concurv undo` does the same.

Markers cover the whole file: a file is either merged line by line
completely or left entirely to you. For large files, a diff tool on `P` (ours), `P.concurv-theirs` or the
marker sections helps.

Many workspaces can save, update and publish at the same time: commits are
serialized internally, and nobody's acknowledged save is ever lost. This is
tested with 2, 5 and 10 concurrent writers, and with processes killed mid-command.

## 4a. Branches

A **branch** is a shared, named reference to a snapshot (`main` is the
first one). A **workspace** is your private copy: files, your own head and
any merge in progress. Every workspace **tracks** exactly one branch.
`publish` moves that branch (compare-and-swap: refused if someone else moved
it first), and `update` brings it in. Several workspaces can track the same
branch. Rules and rationale: `docs/adr/0007-branch-model.md`.

```console
$ concurv branch create feature/login         # at this workspace's head; nothing else moves
$ concurv workspace add ../login --branch feature/login
$ cd ../login && concurv save -m "login form" && concurv publish   # moves feature/login only
$ concurv branch list                         # every branch, its head, who tracks it
```

**Two commands for integrating:**

| Command | Brings in | Then |
|---|---|---|
| `concurv update` | the branch this workspace **tracks** (what others published to it) | `publish` |
| `concurv merge <branch>` | **another** branch, e.g. a finished feature into `main` | `publish` (to your own branch) |

Both use the same merge rules (section 4) and need a clean working copy.
Neither publishes anything by itself.

```console
$ cd ~/code/project                           # tracks main
$ concurv merge feature/login                 # conflicts: --allow-conflicts, or merge --abort
$ concurv publish                             # main now contains the feature
```

**Unpublished saves.** If you saved work on `main` but decide it belongs on
its own branch, put a branch there and move to it in one step. No files
change, and `main` stays where it was:

```console
$ concurv branch create experiment --switch
```

**Switching a workspace to another branch:**

```console
$ concurv branch switch feature/login         # checks out that branch's head
$ concurv undo                                # back to the previous branch and files
```

`branch switch` never loses work. Untracked files that don't collide with
the other branch stay where they are. The switch refuses, with nothing
changed, if:
- a tracked file has unsaved changes, or an untracked or ignored file would
  be overwritten or removed (exit 20);
- a merge is in progress (exit 43);
- your saves would end up on no branch (exit 42). Publish them first, or use
  `branch create <name> --switch`.

If a file can't be written halfway through, the working copy is put back as
it was. If the process is killed, `concurv status` reports an interrupted
switch. Every file then holds its old or its new content, so nothing is
lost: run the same `concurv branch switch <name>` again to finish, or
`concurv restore --snapshot <origin> --force` (shown by `status`) to go back.

**Branch names** follow Git's rules, so they export unchanged
(`concurv git export ../mirror.git --from feature/login`). concurv is
stricter in a few places so names work on every OS:
- ASCII only;
- none of `< > " |`;
- no Windows device names such as `con`;
- no leading `-`, and not `HEAD` or `@`;
- the first parts `refs`, `remote` and `remotes` are reserved;
- no two branches that differ only in case, and not both `x` and `x/y`.

**Deleting and renaming** (ADR 0007 §9):

```console
$ concurv branch rename feat/x feature/x   # workspaces on feat/x follow; no files change
$ concurv branch delete feature/x          # only the name goes; the snapshots stay
```

- `delete` refuses while a workspace tracks the branch (switch it first),
  and refuses a branch whose work no other branch, tag, workspace or
  remote-tracking reference contains (exit 42): merge it, or keep it with
  `concurv tag create archive/x --at feature/x`, then delete. The output
  prints the head, so `concurv branch create <name> --at <id>` restores it.
- Protected branches (§8b) can be neither deleted nor renamed (exit 44).
- Both are local: a relay keeps its branch. The next `fetch` does not bring
  back a branch you deleted or renamed; it only updates
  `remote/<relay>/<branch>` (`concurv branch create <b> --at
  remote/origin/<b>` brings it back). After a rename, `concurv push --branch
  <new>` publishes the new name.
- `undo` cannot reverse `branch create`, `delete` or `rename`: branches are
  shared, and someone may already be using them.

**Words used in this guide.** Your *workspace state* (files, head, merge in
progress) is private. A *branch* here is a reference shared by all
workspaces of the repository; on one machine it is also the published head.
Across machines (§8a), a relay holds the authoritative published heads, and
fetched heads appear as read-only *remote-tracking* references
(`remote/<relay>/<branch>`). They never overwrite your branches, and
divergent work is merged by you before it is published
(`docs/rfcs/0003-sync-protocol.md` §5).

## 4b. Tags

A tag gives a snapshot a name that never moves, for example a release
(ADR 0008):

```console
$ concurv tag create v1.0                 # at this workspace's head
$ concurv tag create v0.9 --at main       # or a branch, another tag, a snapshot id
$ concurv tag list
$ concurv branch create hotfix --at v1.0  # tags work wherever --at does
$ concurv tag delete v0.9                 # prints where it was; the snapshot stays
```

- A name that exists is refused; tags are never moved. To re-point one,
  delete it and create it again (both steps are in the operation log).
- Names follow the branch-name rules above; a tag may have the same name as
  a branch, but then `--at` refuses to guess (exit 51).
- `undo` refuses tag operations, as for `branch create`.
- The first tag upgrades the repository to **format 4**; builds older than
  this one cannot open it afterwards.
- Tags travel to and from Git with `git export --tags` and `git import
  --tags` (§9), and through a relay: `concurv push --tags` creates your
  tags there (only for snapshots the relay has: push the branch first),
  and `fetch` (or `clone`) brings the relay's tags. A relay never moves a
  tag either: if it has the name elsewhere, `push --tags` is refused, and
  `fetch` keeps your local tag and reports the conflict.

## 5. Going back to an earlier snapshot

```console
$ concurv log                                    # find the snapshot id
$ concurv restore --snapshot 1a2b3c4d
```

What `restore` does:

1. It loads and hash-checks the entire target snapshot **before** touching any file.
2. If you have unsaved changes (including new files), it **refuses** with exit
   code 20 and changes nothing.
3. It makes your files match the snapshot: it writes changed files, deletes
   files the snapshot doesn't have, and removes directories left empty.
4. **History does not move.** Your head is still the latest snapshot, so
   `status` now shows the difference. Run `concurv save -m "Revert to …"` to
   record the restored state as a new snapshot on top.

### Overwriting unsaved changes safely: `--force`

```console
$ concurv restore --snapshot 1a2b3c4d --force
Restored working copy to 1a2b3c4d… (2 written, 1 deleted). History unchanged; `concurv save` to record it.
Previous working copy saved as backup snapshot 9f8e7d6c… (recover with `concurv restore --snapshot 9f8e7d6c… --force`).
```

With `--force`, concurv first saves your current working copy, unsaved edits
included, as a **backup snapshot**, and only then overwrites files. To get your
work back, restore the backup id.

Backups don't appear in `concurv log`, because they aren't part of the branch
history. If you lost the printed id, find it in the operation log:

```console
$ concurv op log -n 5
operation 7c1e… (restore)
Date:   2026-11-01 14:02:11 UTC
    restore working copy to 1a2b3c4d…
    head after: 5d6e…
    backup of discarded working copy: 9f8e7d6c…
    (recover with `concurv restore --snapshot 9f8e7d6c… --force`)
```

Simpler: right after a forced restore, `concurv undo` puts the unsaved work
back exactly (next section).

## 6. Undo

`concurv undo` reverses the most recent operation **of the workspace you are
in**, and exactly what that operation changed:

| You last ran | `undo` does |
|---|---|
| `save` | moves your head back to before the save. Your files are **not** touched, so the saved changes show up in `status` as unsaved again |
| `update` | puts your files and head back to before the update (the merge or fast-forward disappears from your workspace) |
| `restore` | puts your files back to what they were before the restore. After `restore --force`, that means your unsaved work comes back |
| `publish` | **refused** (exit 2). Others may already build on what you published; make a new change that reverts it instead |

Run `undo` again to go further back: it skips operations that were already
undone. Undo itself cannot be undone; to return to a state, find its snapshot
in `concurv op log` and `restore` it.

Undo is safe:

- It only ever changes **your** workspace. Other workspaces' heads, their files
  and `main` are never touched, even if they ran commands in between.
- If undoing would overwrite files you changed after the operation (for
  `update` and `restore`), it refuses with exit 20 and changes nothing. Save
  or revert your edits first.
- Nothing is deleted: undone snapshots stay in the operation log.

The first checkout of a new workspace (from `workspace add`) cannot be undone.

## 7. The operation log

`concurv op log` lists every repository operation, newest first: `init`,
`save` and `restore`. Each entry shows its time and description, the head
snapshot after it ran and, for `restore --force`, the backup snapshot of the
work it replaced. `-n N` limits the output, and `--json` gives the full
record, including parent operations and branch heads. The command is
read-only.

The operation log is separate from snapshot history (`concurv log`): a
`restore` adds an operation but no snapshot. Every backup stays recorded there
permanently. Nothing is ever garbage-collected in R1.

## 8. Checking integrity

```console
$ concurv verify          # everything reachable from history
$ concurv verify --full   # additionally every object file, including unreferenced ones
```

`verify` never modifies anything. Exit 0 means intact, 30 means a corrupt object,
and 31 means a missing object. Each problem lists the object id and reason.

## 8a. Syncing between machines (relay)

Machines exchange published branches through a **relay**, `concurvd`, a
small server you run yourself (RFC 0003). The relay holds the
authoritative head of every published branch; it moves only by
compare-and-swap and only forward. Sync never changes your working files:
you integrate fetched work with `update` or `merge`, as on one machine.

**Set up a relay** (on a machine every device can reach):

```console
$ concurvd init /srv/concurv/myproject
Fingerprint (give it to every client): sha256:4f0c…
$ concurvd device add /srv/concurv/myproject laptop     # prints a token once
$ concurvd device add /srv/concurv/myproject ci --scope read
$ concurvd serve /srv/concurv/myproject --listen 0.0.0.0:7461
```

Give each device its token (keep it like a password) and the fingerprint.
The fingerprint is how a client recognizes the relay: copy it over a
channel you trust, never from the relay's own answer. A device whose token
leaks is revoked with `concurvd device revoke <dir> <name>`.

**Clone, or add the relay to an existing repository:**

```console
$ concurv clone concurv://relay.example:7461 myproject \
    --fingerprint sha256:4f0c… --token-file ~/laptop.token
$ concurv remote add origin concurv://relay.example:7461 \
    --fingerprint sha256:4f0c… --token-file ~/laptop.token
```

Adding a remote upgrades the repository to format 2; concurv builds older
than this one can no longer open it.

**Daily flow:**

```console
$ concurv save -m "work"        # private to this workspace
$ concurv publish               # local branch main moves forward
$ concurv push                  # the relay's main moves forward (CAS)
$ concurv fetch                 # download others' work; local main fast-forwards if behind
$ concurv update                # bring it into this workspace
```

**When two machines published offline** (both pushed to their local
`main`), the second push is refused with exit 42: nothing is published
behind your back. Then:

```console
$ concurv fetch                          # main: diverged
$ concurv merge remote/origin/main       # merge the relay's work into this workspace
$ concurv publish && concurv push        # publish the merge, then push it
```

`remote/<remote>/<branch>` is the **remote-tracking reference**: the head
this repository last saw at the relay. Only `fetch` and `push` move it, and
it is listed by `concurv remote list`. If a push races with another
machine's, one wins and the other gets exit 41 (`CONCURV_STALE_HEAD`);
fetch and integrate again. A lost connection during `push` is retried
automatically and never publishes twice.

If `fetch` reports a branch as **rewound**, the relay's head went backwards
(for example, the relay was restored from an old backup). concurv does not
follow it and refuses to push to it; your history is untouched. Decide with
the relay's operator what the right head is.

Exit codes of sync: 70 network, 71/72 protocol, 73 bad or revoked token, 74
missing permission, 75 the relay's certificate does not match the
fingerprint (nothing was sent). See `docs/cli.md`.

## 8b. Protected branches: publish only what passed its checks

A branch can require that every new head passed the project's checks
(RFC 0005). Put a `concurv-policy.json` at the root of the project:

```json
{
  "concurv_policy": 1,
  "protect": ["main"],
  "checks": [ { "name": "test", "argv": ["cargo", "test", "--locked"], "timeout_s": 1800 } ],
  "sandbox": "none"
}
```

The policy that counts is the one in the branch's **current head**: it
starts to apply once you publish it, and a candidate that edits it is
still judged by the published version. From then on:

```console
$ concurv save -m "work"
$ concurv publish                 # exit 44: main is protected, no attestation yet
$ concurv check --trust           # runs the checks on the saved snapshot; prints PASSED/FAILED
$ concurv publish                 # accepted: a passing attestation exists for exactly this head
$ concurv publish --check         # or both in one step
```

- `check` writes the **saved snapshot** into a fresh directory and runs the
  checks there, so unsaved or ignored files never affect the result. Edit
  and save again, and the old attestation no longer applies.
- Checks are commands from the repository and run **without isolation**
  (`"sandbox": "none"` says so). Nothing runs until you approve that
  version of the policy on your machine with `--trust`; a changed policy
  needs a new approval.
- After `update` or `merge`, the merged result needs its own passing
  check: two branches that pass separately can fail together, and then
  nothing is published.
- At a relay, the same policy protects the relay's branch. Pushing to it
  takes a passing attestation from a device with the `attest` scope
  (`concurvd device add <dir> ci --scope read --scope write --scope
  'publish:*' --scope attest`). `push` uploads and vouches for it
  automatically.

## 8c. Coordinating agents: intents, overlaps, events

Several people or agents can work in parallel without stepping on each
other unknowingly (RFC 0006). Everything here is **advisory**: nothing is
ever locked or refused.

```console
$ concurv intent declare 'src/parser/**' --symbol parse --ttl 2h --note "parser rewrite"
$ concurv intent list             # everyone's active intents (this repository and fetched ones)
$ concurv overlaps                # what of my changes collides with others' work
$ concurv intent release          # done (or just let it expire)
$ concurv events                  # the operation log as JSON lines, for tools and agents
```

`overlaps` compares this workspace's changes (saved and unsaved, since the
branch) with other workspaces' unpublished saves, with work on the branch
or the relay that this workspace has not integrated yet, and with intents:

- `path`: the same file is changed elsewhere;
- `conflict`: the line edits overlap or touch, so a merge would conflict;
- `intent`: a changed path matches someone's declared pattern, or a changed
  file uses a declared symbol;
- `dependency`: upstream work changed the definition of a function or type
  (`fn`, `def`, `function`, `class`, `struct`, …) that your changes call. A
  signature change on another machine shows up after `concurv fetch`,
  before you publish code that still uses the old signature.

Intents travel with `push` and `fetch` (each device sends its own). An
intent expires at its time-to-live, also for agents that were offline; an
expired intent is ignored everywhere. Measured accuracy of the warnings is
in `bench/RESULTS.md`: conflict warnings never missed a real conflict in
the test corpus, and dependency warnings miss only calls the code makes
indirectly (through tables or strings), which a lexical rule cannot see.

## 9. Working with Git and GitHub

concurv can write its history into a Git repository, and import Git history,
once or repeatedly. Both require `git` on your `PATH`.

**Export** `main` to Git, for example to push it to GitHub:

```console
$ git init --bare ../project-mirror.git            # once
$ concurv git export ../project-mirror.git          # writes main as Git commits
$ git -C ../project-mirror.git push git@github.com:you/project.git main
```

- Only **published** history (`main` and everything before it, merges
  included) is exported. Unpublished saves in workspaces are not.
- Exporting again only adds new commits. The same snapshot always becomes the
  same Git commit, so repeated exports are safe.
- `--branch B` exports to Git branch `B` instead of `main`; `--from C`
  exports concurv branch `C`.
- `--tags` also writes your tags (§4b) that name snapshots of that history,
  as lightweight Git tags, together with the branch: either everything
  lands or nothing does.
- If the Git branch has commits that concurv doesn't know (someone committed
  in Git directly), export **refuses** with exit 42 and changes nothing in Git.
  Import them first (below), or export to a different branch. A Git tag with
  the same name at another commit also refuses the export.

**Import** a Git branch, to start a repository or to bring in what others
committed in Git:

```console
$ mkdir project && cd project && concurv init
$ concurv git import ../existing-git-repo --tags   # --branch B for another Git branch
# ... later, after more Git commits:
$ concurv git import ../existing-git-repo          # only the new commits
```

- Commits concurv already has (from earlier imports or exports to that
  repository) are recognised and reused; only new commits are imported.
  Their Git ids are remembered, so exporting later continues the same Git
  history.
- `main` (or `--into C`) is created or moved **forward only**. If it has
  work that the Git branch does not contain, import refuses (exit 42) and
  changes nothing. Then import beside it and merge, as with any branch:

  ```console
  $ concurv git import ../repo --into git-main
  $ concurv merge git-main && concurv publish
  ```
- Your files change only if this workspace tracks that branch, was at its
  previous head, and has no unsaved changes. Otherwise the output says
  so; run `concurv update` when you are ready.
- `--tags` imports the Git tags of that history (annotated tags become
  plain tags: their message and tagger are dropped). A concurv tag of the
  same name elsewhere refuses the import.

Import is all-or-nothing. It refuses (exit 23, listing everything) when the
branch contains something concurv cannot represent: submodules, paths named
`.concurv`, file names that aren't valid UTF-8, or non-UTF-8 commit messages.

What survives a round trip: file contents, executable bits, symlinks,
directory structure, merges, authors, messages and (with `--tags`) tags.
Not preserved: separate committer identities, timezones and sub-second
times. Imported Git commits keep their ids when exported back to a Git
repository that has them; exported anywhere else they become new commits
(concurv adds a `Concurv-Change-Id` trailer). Notes and signatures are not
imported. Full details:
`docs/adr/0002-git-interop.md`.

## 10. Troubleshooting and recovery

| Symptom | What it means | What to do |
|---|---|---|
| `error[CONCURV_DIRTY_WORKSPACE]` (exit 20) on `restore` or `update` | You have unsaved changes | `concurv save -m …` first. For `restore` only: `--force` keeps a backup snapshot and overwrites |
| `status` shows **WARNING: a restore … was interrupted** | A restore, `update` or `workspace add` checkout crashed or was killed midway; some files are from the old state and some from the new | Re-run the printed command: `concurv restore --snapshot <target> --force`. If a backup id is shown, your pre-restore work is safe in that snapshot |
| `error[CONCURV_LOCKED]` (exit 40) | Another concurv process held the repository or workspace lock for the whole 30 s wait | Retry. Commands normally wait for each other automatically. The lock is held by the OS and released automatically when that process exits, even if it crashed, so there's nothing to delete |
| `error[CONCURV_DIVERGED]` (exit 42) on `publish` | Someone published to `main` after your last `update` | `concurv update`, test, `concurv publish` again. Nothing was changed |
| `error[CONCURV_CONFLICT]` (exit 43) on `update` | Both sides changed the same lines of a file, or a binary file (paths are listed), a file is a directory on the other side, the histories have no single merge base, or a merge is already pending | Nothing was modified. Same-file conflicts: `concurv update --allow-conflicts` and resolve (section 4). A pending merge: finish it with `save` or `update --abort` |
| `error[CONCURV_CONFLICT]` (exit 43) on `save` | A merge is pending and some conflicts are unresolved (listed: markers remain or side files still exist) | Finish resolving, delete the `*.concurv-conflict` / `*.concurv-theirs` files, save again. Or `concurv update --abort` |
| `error[CONCURV_INVALID_ARGUMENT]` (exit 2) on `undo` | Your most recent operation was a `publish` (or `workspace add`), which undo never reverts | Make a new change that reverts the published work, save and publish it |
| `error[CONCURV_NOT_FOUND]` (exit 50) on `undo` | Nothing left to undo in this workspace | — |
| `error[CONCURV_EXTERNAL_TOOL]` (exit 60) on `git export` / `git import` | `git` is not installed, or a git command failed (its message is shown) | Install Git and make sure `git --version` works in the same terminal |
| `error[CONCURV_DIVERGED]` (exit 42) on `git export` | The Git branch has commits that aren't in concurv's `main` | Nothing changed in Git. Export to another branch (`--branch`), or bring those changes into concurv by hand |
| `error[CONCURV_UNSUPPORTED_FILE_TYPE]` (exit 23) on `git import` | The Git history contains something concurv cannot represent (listed) | Nothing was imported. Remove submodules / rename the paths in Git first |
| `error[CONCURV_CONCURRENT_MODIFICATION]` (exit 21) | A file changed while concurv was reading or replacing it (e.g. an editor or build was writing) | Let the other program finish, then retry. Nothing was recorded or overwritten from the inconsistent read |
| `error[CONCURV_UNSUPPORTED_FILE_TYPE]` (exit 23) | A FIFO, socket or device file, or (on Windows) a symlink or junction, is in the project | Move it out of the project directory. R1 cannot version it, and refuses rather than silently skipping it |
| `error[CONCURV_INVALID_PATH]` (exit 22) on `save` | A file name is not valid UTF-8, or contains characters concurv forbids | Rename the file |
| `status` seems to miss a change on a network or FUSE filesystem | That filesystem may not update the inode change time, which the stat cache relies on (Unix only, ADR 0005) | `concurv status --no-cache` hashes every file. `save` never trusts the cache, so snapshots are always correct |
| `error[CONCURV_INVALID_PATH]` on `restore` (Windows/macOS) | The snapshot has names this OS can't create safely (`CON`, `a:b`, names differing only in case, …) | Restore on Linux, or rename those files there and save. Nothing was modified |
| `verify` reports `CONCURV_OBJECT_CORRUPT` / `CONCURV_OBJECT_MISSING` | Storage damage (disk fault, manual edits, an aborted copy) | See below |
| `error[CONCURV_UNSUPPORTED_FORMAT]` (exit 12) | The repository was written by an incompatible concurv version | Use the matching concurv version. concurv never guesses at unknown formats |
| `error[CONCURV_NOT_A_REPO]` (exit 10) | No `.concurv/` in this directory or any parent | `concurv init`, or `cd` into the project. Repositories made by pre-rename `arc` builds (with an `.arc/` folder) aren't recognised (ADR 0003) |

### Corruption

1. **Stop and copy the whole project directory**, including `.concurv/`,
   somewhere safe before trying anything else.
2. Run `concurv verify --full` and note the affected ids.
3. If the damaged object is file content that still exists unchanged in your
   working copy, the next `concurv save` rewrites it. Any corrupt copy is moved
   to `.concurv/quarantine/` and kept for diagnosis, never deleted. Re-run
   `verify` to confirm.
4. concurv never "repairs" by guessing. Restoring a snapshot whose objects are
   damaged fails before any file is touched.

### Backing up a repository

Copy the entire project directory while no `save` or `restore` is running.
Objects are immutable and the head is a single small file, so a copy taken
between commands is consistent.

## 11. Scripting and agents

Add `--json` to any command for one machine-readable JSON document on stdout,
with `concurv_json_version`, `ok`, `data` and, on failure, `error.code`. Branch
on the exit code or `error.code`, never on human text. The schema and the
full error/exit-code table are in `docs/cli.md`. `-C <dir>` runs a command as
if started in `<dir>`.

**Configuration.** Two keys, set per repository or for your user account:

```console
$ concurv config set author "Ada Lovelace <ada@example.com>" --global   # user file
$ concurv config set remote backup        # this repository: fetch/push default
$ concurv config list                     # effective values and where they come from
$ concurv config unset remote
```

The repository file (`.concurv/config.json`) applies to all its
workspaces and wins over the user file; `CONCURV_AUTHOR` wins over both.
The user file is `%APPDATA%\concurv\config.json` on Windows and
`~/.config/concurv/config.json` elsewhere (`$CONCURV_CONFIG` overrides the
path; set it to an empty value to use no user file, for example in CI or
for an agent that must not inherit your settings). Unknown keys and
malformed files are errors, never silently ignored.

**Shell completion:**

```console
$ concurv completions bash > ~/.local/share/bash-completion/completions/concurv
$ concurv completions zsh > "${fpath[1]}/_concurv"
$ concurv completions fish > ~/.config/fish/completions/concurv.fish
PS> concurv completions powershell | Out-String | Invoke-Expression   # add to $PROFILE
```

## 12. Platform notes

| | Linux | macOS | Windows |
|---|---|---|---|
| Tested | every change, local gate in WSL (x86_64) | hosted CI until 2026-11-02 (Apple Silicon, APFS); not covered by the local gate | every change, local gate (NTFS) |
| Executable bit | recorded | recorded | not available; a file keeps the kind recorded in the head |
| Symlinks | versioned as links, never followed | same | rejected (symlinks and junctions) |
| Directory fsync after rename | yes | yes | not available; relies on NTFS journaling |
| Non-UTF-8 file names | rejected by `save` | cannot exist (APFS refuses them) | rejected (unpaired surrogates) |
| Unsafe-name checks on restore | none needed | case collisions | reserved names, `<>:"\|?*`, trailing dot/space, case collisions, `CONCUR~n` |
| `status`/`diff` stat cache (ADR 0005) | yes | yes | no (always hashes every file) |
