# ADR 0002 — Git interoperability boundary (design only)

Status: **Accepted direction; not implemented** (targeted for R2 minimum
subset, R6 for GitHub workflows). Date: 2026-11-01.

## Why concurv ids are not Git ids

Git object ids are SHA-1 (or SHA-256) over `"<type> <len>\0<body>"` with Git's
own tree mode and ordering rules (directories sort as if suffixed by `/`).
concurv ids are BLAKE3 derive-key hashes over a different canonical encoding that
also covers concurv-only fields (ChangeId, operation views). The same file content
therefore has *different* ids in the two systems; treating them as
interchangeable would be a correctness bug. The bridge keeps an explicit,
persisted **id map** `ArcId ↔ GitId` per object kind.

## Boundary

A separate crate `concurv-git` (not yet created), using `gix` (gitoxide), behind a
trait so the kernel does not depend on Git:

- **Export**: concurv snapshot → Git commit. Blob bytes identical; `file` →
  `100644`, `executable` → `100755`, `symlink` → `120000`, `tree` → `040000`.
  Author/time map to Git author and committer. ChangeId is written as a commit
  trailer `Concurv-Change-Id: <hex>` so it survives round-trips.
- **Import**: Git commit → concurv snapshot, verifying every Git object it reads.

## Known limitations (must be reported, not hidden)

| Git feature | concurv R1 model | Handling |
|---|---|---|
| Submodules (`160000` gitlinks) | not representable | import refuses unless `--skip-submodules`, listed in report |
| Empty directories | not versioned (same as Git) | n/a |
| Non-UTF-8 paths | rejected | import fails with path list |
| Separate committer vs author, timezone offset | single author + UTC ms | preserved in a native side record, lossy in concurv view |
| GPG/SSH signatures | not modelled | dropped on import, reported; export cannot re-sign |
| Merge commits with >1024 parents | rejected by limit | reported |
| Notes, replace refs, grafts, shallow history | unsupported | reported |
| concurv operations, backups, conflicts | no Git equivalent | never exported; local-only metadata |

A round-trip is only claimed supported for the subset covered by a
round-trip test suite.
