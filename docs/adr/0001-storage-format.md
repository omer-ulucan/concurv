# ADR 0001 — Native typed BLAKE3 object store with a single-pointer operation log

Status: **Accepted** (R1). Date: 2026-11-01.

## Context

concurv needs immutable, content-addressed storage with typed, domain-separated ids
(RFC 0001), explicit durability (RFC 0002) and a future Git bridge (ADR 0002).

## Options

1. **Git object format as native storage** (SHA-1/SHA-256, zlib loose objects
   and packs). + free interop with Git tooling. − Git ids are not typed/domain
   separated, tree mode rules are Git-specific, SHA-1 is legacy, and concurv
   concepts (operations, change ids, conflicts, attestations) would need
   side-channel storage anyway.
2. **Native typed BLAKE3 store** with an explicitly specified byte encoding.
   + exact control of canonical form, limits and validation; fast hashing;
   room for new kinds. − Git interop requires translation (ADR 0002).
3. **SQLite for everything** (objects as rows). + one transactional store.
   − large blobs in SQLite are awkward to stream; single DB file is a larger
   corruption blast radius; adds a C dependency to the kernel.

Metadata/refs options: (a) SQLite WAL table of refs + ops; (b) per-ref files
with CAS; (c) **operation objects that each carry the full View, plus one
mutable `op_head` file** (the Jujutsu "view" design).

## Decision

- Option 2 for objects: loose files `objects/ab/<64-hex>`, uncompressed,
  canonical bytes exactly as hashed. Compression/packing can be added later
  **without changing ids** because ids are over canonical bytes, not stored
  bytes.
- Option (c) for refs: one atomic rename commits an entire multi-ref change.
  No SQLite dependency in R1. The cost is that each operation re-encodes the
  whole View — fine while the number of refs is small; R2+ can store the View
  as a separate object referenced by id if needed (a new format version).
- Hand-written encoder/decoder; **no generic serializer participates in
  hashing** (serde is used only for CLI JSON output).

## Versioning and migration

- Object magic `CNV1` encodes object-format version 1; repository file
  `.concurv/format` encodes the repository layout version.
- A reader encountering an unknown magic or repository format refuses to
  operate (`CONCURV_UNSUPPORTED_FORMAT`) rather than guessing.
- A future format N+1 will ship an explicit `concurv migrate` that rewrites into a
  new store and keeps a mapping from old ids to new ids; old ids are never
  silently reinterpreted.
- **Migration note, repository format 1 → 2 (R3, 2026-11-12).** Format 2
  adds an optional trailing view extension to operations (remote-tracking
  references, RFC 0001 §3.4). Object magic and every object id stay the
  same, and every format-1 operation decodes unchanged, so no rewrite is
  needed: `concurv remote add` (and `clone`) replaces `.concurv/format`
  atomically under the repository lock. Downgrading is not supported:
  builds before R3 refuse a format-2 repository (`CONCURV_UNSUPPORTED_FORMAT`).

## Consequences

- Every `save` currently re-hashes all working files (no stat cache), and every
  dedup hit re-verifies the existing object. Correct but O(repo size); a
  verified stat cache is an R2/R6 performance task (must not become the only
  correctness oracle — cf. jj #10509).
- Loose objects only: large repositories will produce many files. Packing is an
  R7 task.
