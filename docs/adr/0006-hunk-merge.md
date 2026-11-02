# ADR 0006 — Line-level (hunk) merge in `update`

Status: **Accepted, implemented** (R2.5, P1-1). Date: 2026-11-01.

## Context

R2's `update` merged at file level only (RFC 0004 §7): any file changed on
both sides was a conflict, even when the edits were far apart. That is the
most common case when several agents or developers work on one codebase, and
R3 reconciliation across machines needs the same merge. Git, Mercurial and
Jujutsu all merge non-overlapping hunks.

## Decision

1. **When.** Only for a path that exists in base, ours and theirs, is a
   regular file (plain or executable) in all three, and changed differently
   on both sides. Symlinks, binary files (NUL byte), invalid UTF-8, and files
   over 8 MiB (the text-diff limit) stay file-level conflicts. So do
   add/add, delete/modify and file/directory conflicts.
2. **Algorithm.** A conservative diff3. Each side's changes against the base
   are computed (Myers, per line, terminators kept) and grouped into hunks.
   Two hunks from different sides **conflict if they overlap or touch**, that
   is, if no unchanged base line separates them, unless they are identical,
   in which case the change is taken once. This is stricter than Git, which
   merges some adjacent changes. Being stricter can only add conflicts, never
   produce a wrong merge.
3. **Bytes.** Lines keep their exact terminators, so CRLF files, mixed
   endings and a missing final newline merge byte-exactly. No normalization.
4. **Executable bit.** It merges like a file-level change: one side changing
   it wins. Both sides changing it differently cannot happen with two
   values.
5. **All or nothing per file.** A file merges cleanly or stays a conflict as
   a whole; there are no partial merges inside a file. With
   `--allow-conflicts`, the existing conflict model (RFC 0004 §11) is
   unchanged: only the remaining conflicts get marker or side files.
6. **No format change.** The merged content is an ordinary blob in the merge
   snapshot `{parents: [W, M]}`. `UpdateOutcome.content_merged` (CLI JSON
   `content_merged[]`) lists the files merged line by line.

## Consequences

- Disjoint edits to one file no longer block `update`.
- A clean textual merge is still not a verified merge (RFC 0004 §7): build
  and test after `update`, before `publish`.
- Properties tested (`textmerge.rs`): one side unchanged gives the other
  side exactly, the same change on both sides gives that change, and the
  merge is symmetric. Integration tests: `tests/hunk_merge.rs`.
