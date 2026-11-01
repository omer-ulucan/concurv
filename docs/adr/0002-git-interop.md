# ADR 0002 — Git interoperability boundary

Status: **Accepted; minimum subset implemented** in R2 slice 2d (crate
`concurv-git`, commands `concurv git export` / `concurv git import`).
Date: 2026-11-01. GitHub-oriented workflows (push/pull automation) remain R6.

## Why concurv ids are not Git ids

Git object ids are SHA-1 (or SHA-256) over `"<type> <len>\0<body>"` with Git's
own tree-mode and ordering rules (directories sort as if suffixed by `/`).
concurv ids are BLAKE3 derive-key hashes over a different canonical encoding
that also covers concurv-only fields (ChangeId, operation views). The same file
content therefore has *different* ids in the two systems, and treating them
as interchangeable would be a correctness bug. The bridge keeps an explicit
map from snapshot to Git commit (`.concurv/git-map`).

## Decision: drive Git's own streaming tools

The original plan was to link `gix` (gitoxide). The implementation instead
runs the `git` command-line tool's **`fast-import`** (export) and
**`fast-export`** (import) streams:

| Option | For | Against |
|---|---|---|
| `gix` library | no runtime dependency on `git` | large dependency; concurv would be responsible for correct object, pack and index handling |
| hand-written Git object writer | no dependency | loose objects only; must reimplement SHA-1/SHA-256 object rules and read packs |
| **`git fast-import` / `fast-export`** | Git itself writes and reads every object (either hash, packs, no CRLF conversion in plumbing); one streamed process per command; the formats are stable and documented | requires `git` on `PATH`; parsing a text protocol |

The `git` process runs hermetically: `GIT_DIR`, `GIT_WORK_TREE`,
`GIT_INDEX_FILE` and similar variables are removed, and every call uses
`git -C <repo>`. A missing or failing `git` gives `CONCURV_EXTERNAL_TOOL`
(exit 60).

## Export (`concurv git export <repo> [--branch main]`)

- Exports **every snapshot reachable from `main`** (merges included) in
  parent-first order. Snapshots already present in the target repository are
  referenced by commit id, so exports are incremental.
- Mapping: `file` → `100644`, `executable` → `100755`, `symlink` → `120000`;
  blob bytes are identical. Commits are written to the private ref
  `refs/concurv/export` (deleted afterwards), then **`refs/heads/<branch>` is
  moved with a compare-and-swap** (`git update-ref <ref> <new> <old>`). If the
  Git branch has commits that are not part of `main`'s history, export refuses
  with `CONCURV_DIVERGED` and changes nothing in Git.
- Each commit depends **only on data that survives a round trip**: tree,
  parents, author, whole seconds, the message, and a final trailer
  `Concurv-Change-Id: <hex>`. Export is therefore deterministic: the same
  snapshot always becomes the same Git commit, and
  concurv → Git → concurv → Git reproduces identical commit ids (tested).
  `.concurv/git-map` is just a cache: deleting it costs a re-export, never
  different commits.

## Import (`concurv git import <repo> [--branch main]`)

- Imports a Git branch with its full history into a **fresh** repository
  (`main` and the current workspace empty), publishes the head as `main`, and
  checks it out. Importing into existing history is not supported.
- Each commit becomes a snapshot with the same tree contents (identical
  concurv tree ids for the same files) and the same parent structure. The
  ChangeId comes from the `Concurv-Change-Id` trailer if present (the trailer
  is removed from the message), otherwise it is derived deterministically from
  the Git commit id.
- Import is all-or-nothing: every problem is listed, and nothing is published
  unless the whole branch can be represented.

## What is preserved and what is not

| Git / concurv feature | Behaviour |
|---|---|
| file bytes, executable bit, symlinks, nested directories, Unicode paths, merges | preserved both ways |
| author name and email | preserved (`Name <email>`; an author string without email exports with an empty email) |
| committer | export: set equal to the author. Import: **dropped** |
| timestamps | export: **truncated to whole seconds**, timezone `+0000`. Import: the timezone is **dropped** (concurv stores UTC milliseconds) |
| message | trailing newlines are normalized away. On export a `Concurv-Change-Id` trailer is appended; on import it is removed again |
| Git commit ids of *native* Git history | not preserved after a round trip through concurv (the ChangeId trailer, committer and timezone change) |
| submodules (`160000` entries) | import **refused**, listed |
| paths concurv rejects (`.concurv`, invalid or non-UTF-8 names) | import **refused**, listed |
| non-UTF-8 commit messages (`encoding` header) | import **refused**, listed |
| tags, notes, signatures, replace refs, grafts, shallow history | not imported (tags and notes are skipped, signatures stripped); export writes no tags |
| empty directories | not versioned on either side |
| concurv operations, workspaces, backups, pending merges | never exported; local-only metadata |
| concurv snapshots not reachable from `main` (unpublished work) | not exported |

A round trip is only claimed for what the tests in
`crates/concurv-git/tests/bridge.rs` cover.

## Typical GitHub workflow

```console
$ git init --bare ../project-mirror.git
$ concurv git export ../project-mirror.git
$ git -C ../project-mirror.git push git@github.com:you/project.git main
```

Pulling changes made on the Git side back into existing concurv history is
not supported yet: `export` refuses rather than overwriting them.
