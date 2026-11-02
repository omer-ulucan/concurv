# ADR 0005 — Stat cache for `status` and `diff`

Status: **Accepted, implemented** (R1.5, P0-7). Date: 2026-11-01.

## Context

Until R1.5, `status` and `diff` hashed every tracked or untracked file on every
run. Measured baseline (`bench/RESULTS.md`): 0.8 s for a clean `status` on
10k files / 84.5 MiB on Windows, scaling linearly with tree size. Git and
Jujutsu avoid most of that cost with a stat cache. Getting one wrong is a
known source of silent data-loss bugs: jj #10509 reports a same-size,
same-mtime rewrite shown as unchanged, and Git has the "racy git" problem.
Under the V2 rules a cache is disposable and is never the authority for a
published snapshot.

## Decision

1. **Who writes, who reads.** Only `save` writes the cache. It hashes every
   file in full and replaces `.concurv/stat-cache` of its own workspace after
   its commit point (also after a no-op save), under the workspace lock.
   `status` and `diff` only read it, so invariant R1-I4 (they never write)
   is unchanged. `save`, `restore`, `update`, `undo` and workspace checkout
   never consult the cache: every safety check and every snapshot uses
   content hashes.
2. **Platforms.** Trusted on Unix only. There the kernel sets the inode change
   time (`ctime`) on every content or metadata change, and user code cannot
   set it. On Windows, `std` exposes no such time and NTFS timestamps can all
   be set by user code, so a same-size, same-mtime rewrite would be
   undetectable. Windows therefore neither writes nor reads a cache and keeps
   full hashing.
3. **Key.** An entry is keyed by the workspace path and must match device,
   inode, size, full mode, `mtime` and `ctime` (both with nanoseconds)
   **exactly**. Only regular files are cached, never symlinks. The
   executable bit is always taken from the current `lstat`.
4. **When an entry is recorded.** Only if the identity seen by `lstat`
   before opening, by `fstat` on the open file, and by `lstat` after hashing
   is the same, and both `mtime` and `ctime` are older than the start of the
   save minus a **racy window** (default 2 s). A file written within the
   window is hashed again by every `status` until a later save. A file that
   changes between the two reads of a save is not recorded.
5. **Failure model.** A missing, unreadable, truncated, corrupt or
   wrong-version cache (BLAKE3 checksum over the whole file, magic
   `CNVSTC1`) is ignored and rebuilt by the next save. Writing it is best
   effort: temp file in `ws-tmp/`, then `rename`, no fsync, and a write
   error never fails the save. The file is not part of the repository format
   (RFC 0001) and may be deleted at any time.
6. **Escape hatch.** `status --no-cache` and `diff --no-cache` hash every
   file.

## Consequences

- A file changed after the last save, in any way that updates its `ctime`
  or other identity, is hashed again. That covers rewrites with a restored
  mtime, rename-over, delete-and-recreate and `chmod`. Unchanged files cost
  one `lstat` each.
- Residual risk, accepted and documented: a filesystem that does not update
  `ctime` (some FUSE or network filesystems), or a wrong system clock
  combined with a change in the same timestamp tick as the save's own stat,
  could make an entry match a changed file. Neither can affect a snapshot
  (`save` never trusts the cache); use `--no-cache` on such filesystems.
- Each workspace has its own cache; workspaces never share entries.

## Verification

`crates/concurv-workspace/tests/stat_cache.rs` (false-clean suite):
same-size + same-mtime rewrite, change right after save (racy window),
repeated edits, rename-over, delete-and-recreate, corrupt/truncated/missing
cache, status never writing the cache, `--no-cache`, two workspaces, symlink
retarget and mode change (Unix), only settled files cached (Unix). Unit tests
in `statcache.rs` cover the racy guard, the identical-identity rule, and
rejection of every single-bit flip and truncation of an encoded cache. These
run on Linux, macOS and Windows in CI. Before/after numbers:
`bench/RESULTS.md`.
