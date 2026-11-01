# ADR 0003 — Rename the project from ARC to concurv

Status: **Accepted**. Date: 2026-11-01. Decided by: project owner.

## Context

The project started under the working name **ARC** (`arc`). The roadmap
(`docs/ARC_Research_and_Roadmap_2026.md`) flagged the name as provisional
because availability and executable collisions had not been checked. The owner
chose the permanent name **concurv**, short for *concurrent, agent-native
version control*.

The name appears in two kinds of places:

1. **Cosmetic:** crate names, docs, messages. Renaming these is free.
2. **Format and contract identifiers:** the metadata directory, object magic
   bytes, hash domain string, repository format file, error codes, the JSON
   version field and environment variables. Renaming these changes the on-disk
   format, every object id and the machine-readable CLI contract.

## Options considered

| Option | Consequence |
|---|---|
| A. Rename code and docs only, keep `ARC1` / `.arc` / `ARC_*` as legacy identifiers | Ids stay stable, but users and agents see a mismatched name forever (`concurv` writes `.arc/`, emits `ARC_LOCKED`). |
| **B. Rename everything, keep format version 1** | Every object id changes; repositories created before the rename cannot be opened. Acceptable only because nothing has been released. |
| C. Rename everything and bump to format version 2 | Implies v1 repositories exist and need migration. None do, so the bump would only add noise. |

## Decision

**Option B.** At the time of the decision no repository existed outside test
temp directories and local demos, and no binary had been distributed. This is
the last point at which format identifiers can change without a migration.

The binary is named `concurv` (full name, not an abbreviation such as `cv`, to
avoid collisions with existing tools). Users may define their own shell alias.

## Identifier mapping

| Item | Before | After |
|---|---|---|
| CLI binary | `arc` | `concurv` |
| Crates | `arc-core`, `arc-objects`, `arc-refs`, `arc-ops`, `arc-workspace`, `arc-cli` | `concurv-core`, `concurv-objects`, `concurv-refs`, `concurv-ops`, `concurv-workspace`, `concurv-cli` |
| Error type | `ArcError` | `ConcurvError` |
| Metadata directory | `.arc/` | `.concurv/` |
| Init staging directory | `.arc-init-<random>` | `.concurv-init-<random>` |
| NTFS short-name alias guard | `ARC~<n>` | `CONCUR~<n>` (the 8.3 alias Windows generates for `.concurv`) |
| Object magic (RFC 0001 §3) | `ARC1` (`41 52 43 31`) | `CNV1` (`43 4e 56 31`) |
| BLAKE3 derive-key context | `arc 2026-11 object v1` | `concurv 2026-11 object v1` |
| Repository format file | `arc-repo-format 1` | `concurv-repo-format 1` |
| Restore marker header | `arc-restore 1` | `concurv-restore 1` |
| Error codes | `ARC_*` (e.g. `ARC_DIRTY_WORKSPACE`) | `CONCURV_*` (e.g. `CONCURV_DIRTY_WORKSPACE`) |
| JSON version field | `arc_json_version` | `concurv_json_version` |
| Author environment variable | `ARC_AUTHOR` | `CONCURV_AUTHOR` |
| Future relay (RFC 0003) | `arcd` | `concurvd` |
| Future Git commit trailer (ADR 0002) | `Arc-Change-Id` | `Concurv-Change-Id` |

**Unchanged:** exit codes, JSON field layout, byte layout, limits and all
semantics. Only the identifier strings above changed.

## Consequences

- All golden hash vectors were re-pinned (RFC 0001 §8). The independent
  byte-layout test (`hash_matches_spec_byte_layout`) confirmed the new `hello\n`
  vector before re-pinning, so the change is exactly the magic and context.
- A repository created by a pre-rename build has a `.arc/` directory, which a
  current build does not look for. It reports `CONCURV_NOT_A_REPO` and leaves
  the directory untouched. Its objects would fail the magic check with
  `CONCURV_OBJECT_CORRUPT` ("bad object magic"). No migration tool is provided
  because no such repositories were ever distributed.
- The source documents `docs/ARC_Research_and_Roadmap_2026.md` and
  `prompts/START_WITH_CLAUDE_CODE.md` keep their original text and filenames as
  historical inputs. The roadmap has a one-line rename note at the top, and
  "ARC"/`arc` in either document means concurv.
- Trademark, domain and package-registry (crates.io) availability of
  `concurv` has **not** been checked. This must be done before any public release.
- From now on, any change to an identifier in the "After" column is a format or
  contract change and follows the versioning rules in ADR 0001 and `docs/cli.md`.
