# Compatibility and support (from 0.1.0)

From version 0.1.0, concurv makes the promises below. They are tested by
the gates recorded in `docs/ROADMAP_STATUS.md`; anything not listed here is
not promised.

## Versions

concurv follows [Semantic Versioning](https://semver.org/). While the major
version is 0:

- a **patch** release (0.1.x) only fixes bugs and documentation; it never
  changes a format, a JSON field, an exit code or a command's meaning;
- a **minor** release (0.2.0, …) may add commands, flags, JSON fields, error
  codes and formats, and may change CLI behaviour. Every such change is
  listed under *Changed* in `CHANGELOG.md`; a change that can break a
  script is marked **Breaking**.

## Your data

- Every later concurv release opens repositories of **object format 1**
  and **repository formats 1 to 4** (`docs/rfcs/0001-vcs-semantics.md`),
  workspace format 1 and relay format 1, and reads them exactly as this one
  does. Content ids never change meaning.
- A feature that needs more than a format allows gets a **new format
  number**. The upgrade happens in place, atomically, and only when you
  first use that feature (as `remote add`, `check` and `tag create` do
  today). It never rewrites objects.
- **Downgrades are not supported**: an older build refuses a newer format
  with `CONCURV_UNSUPPORTED_FORMAT` (exit 12) instead of misreading it. Back
  up a repository (copy it while no command runs) before trying a new
  release on it if you might want to go back.
- `.concurv/git-map`, the stat cache and workspace locator files are
  caches: deleting them never loses history.

## Scripts and agents

- `--json` output is `concurv_json_version` 1. Within version 1, a field is
  never removed and never changes meaning; new fields may appear, so
  consumers must ignore fields they do not know.
- Exit codes and `CONCURV_*` error codes (`docs/cli.md`) keep their meaning;
  new codes may be added in minor releases. Branch on them, never on human
  text, which may change in any release.
- The event stream (`concurv events`) is `concurv_event` 1, with the same
  rule as the JSON output.

## Between machines

- The relay protocol is `concurv-sync` **version 1** (RFC 0003). A client
  and a relay of different 0.x releases negotiate a common version; a
  release that drops version 1 is a minor release and says so.
- Requests added within version 1 (`attest`, `intents`, `tags`) are
  optional: a client keeps working with a relay that does not know them.
  Intents and fetched tags are then simply absent; `push --tags` reports
  that the relay does not keep tags.
- Mixing releases is supported in both directions as long as no repository
  is upgraded to a format the older release cannot open.

## Platforms

| Tier | Platform | Meaning |
|---|---|---|
| 1 | Windows 11 x86_64 (NTFS) | every release passes the full gate on it; binaries are published |
| 1 | Linux x86_64 (static musl binary; gated on ext4) | every release passes the full gate on it; binaries are published |
| 2 | macOS (Apple Silicon and x86_64) | builds from source (`cargo install`); the full gate last ran there on 2026-11-02 (R2.5); sync, protected branches, coordination and tags have **not** run on macOS |
| 3 | anything else Rust supports | may build; never tested |

Repositories on network filesystems or in cloud-synced folders (OneDrive,
Dropbox, iCloud Drive) are not supported: their rename and flush behaviour
is outside the durability guarantee (RFC 0002).

The minimum supported Rust version for building is **1.89**; raising it
happens only in a minor release.

## Security fixes

Only the latest release receives fixes (`SECURITY.md`).
