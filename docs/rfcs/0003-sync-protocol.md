# RFC 0003 — Sync protocol, version 1 (R3)

Status: **Implemented** (2026-11-12) in `crates/concurv-sync`, the `concurvd`
relay and the `concurv remote | fetch | push | clone` commands. Revision 2:
the draft of R0 is replaced by the protocol as built; §9 lists what version 1
deliberately does not do. Normative for protocol version 1.

## 1. Roles

- **Client** — a concurv repository on a device (PC A, PC B, …).
- **Relay (`concurvd`)** — a small self-hostable server for **one
  repository**. It stores objects and is the **single authority** for the
  *published* heads of that repository's branches. It is a coordinator,
  not a consensus participant.

Properties: offline work is always possible; offline heads may diverge;
only the relay's compare-and-swap decides a published head; published heads
only move forward; nothing a peer sends is executed or trusted without
verification; sync never changes working files.

## 2. Visibility levels

| Level | Effect | Mutates working files? |
|---|---|---|
| `awareness` | ephemeral presence/intent events (R5, not implemented) | never |
| `fetch` | download missing objects and the relay's heads into remote-tracking references `remote/<remote>/<branch>`; fast-forward local branches that are strictly behind | never |
| `apply` | `concurv update` / `concurv merge remote/<remote>/<branch>` in a private workspace | yes, with the normal restore safety rules |
| `push` | the relay's compare-and-swap of a published head | never |

## 3. Identity, authentication, authorization

1. **Transport** — TCP, then **TLS 1.3 only** (rustls, `ring` provider).
   Default port 7461. URL: `concurv://host[:port]` (IPv6 in brackets).
2. **Relay identity** — `concurvd init` generates a self-signed ECDSA P-256
   certificate. Clients **pin** its fingerprint, `sha256:` followed by the
   hex SHA-256 of the certificate's DER, given to them out of band
   (`concurvd fingerprint`). No certificate authority, no host-name check.
   The handshake signature is verified against the pinned certificate, so
   only the holder of the relay's private key completes it. A mismatch
   fails the handshake with `CONCURV_RELAY_IDENTITY` (exit 75) **before any
   application data, in particular the token, is sent**.
3. **Device identity** — `concurvd device add <dir> <name>` issues a token,
   `cvt1_` + 64 hex digits (32 bytes from the OS CSPRNG), shown once. The
   relay stores only `BLAKE3-derive-key("concurv 2026-11 relay device
   token v1", token bytes)` and compares hashes in constant time. Revocation
   (`concurvd device revoke`) takes effect at the device's next connection.
   Any failure is the same `CONCURV_AUTH_FAILED` (exit 73), answered after a
   fixed delay (250 ms) to slow guessing.
4. **Scopes** — `read` (heads, objects, `missing`), `write` (upload
   objects), `publish:<branch>` or `publish:*` (publishing also requires
   `write`). A request outside the device's scopes fails with
   `CONCURV_PERMISSION_DENIED` (exit 74) and changes nothing.

## 4. Framing and messages

After the handshake both sides exchange **frames**: `u8 type`, `u64
length` (big-endian), then `length` bytes.

| Type | Content | Limit |
|---|---|---|
| 1 `message` | one JSON object with a `type` field; unknown fields or types are refused | 1 MiB |
| 2 `object` | one object in its canonical encoding (RFC 0001 §3) | non-blob 64 MiB (RFC 0001 §5); blob: receiver's `max_blob`, default 4 GiB |

The receiver of an object computes its id from the bytes; ids claimed by a
peer are never trusted. Only **blob, tree and snapshot** objects travel;
operations, conflicts and anything else are refused (`CONCURV_WRONG_OBJECT_KIND`).

The client sends requests and reads one response each:

| Request | Response | Notes |
|---|---|---|
| `hello {protocol: "concurv-sync", versions: [1], object_format: 1, token}` | `welcome {version, relay, device, scopes, max_blob}` | first message; no common version → `CONCURV_PROTOCOL_MISMATCH` (exit 72) |
| `heads {}` | `heads {branches: {name: {head, seq}}}` | `seq` = publication sequence number of the branch, 1 for its first publish |
| `fetch {wants, haves}` | `pack {count}`, then `count` object frames | ≤ 1024 wants, ≤ 4096 haves; every object reachable from `wants`, minus the history of `haves` and the trees of the haves present at the relay; referenced objects come before objects referencing them |
| `missing {ids}` | `missing {ids}` | which of ≤ 4096 objects the relay lacks |
| `upload {count}` | `ready {}`, then the client sends `count` object frames, then `stored {received}` | ≤ 10,000 per batch; a refusal comes before `ready`, so no frame is in flight |
| `publish {request_id, branch, expected, proposed}` | `published {branch, head, seq, replayed}` | §6 |
| `bye {}` | — | |
| (any failure) | `error {code, message, observed?, seq?, missing[]}` | `code` is a stable error code (`docs/cli.md`) |

A malformed or unexpected message, an object frame where a message belongs,
or any failure while object frames are in flight ends the connection after
an `error` response (`CONCURV_PROTOCOL`, exit 71, or the integrity code of
the bad object). Other failures (stale publish, unknown snapshot, missing
objects, permission) are answered and the session continues. Connections
idle for 120 s are closed; at most 64 are served at once.

## 5. Branch references and reconciliation

Four kinds of state take part; only the relay's is authoritative across
machines (ADR 0007 §8):

| Kind | Stored | Moved by |
|---|---|---|
| Workspace state (files, workspace head, pending merge) | the workspace | local commands in that workspace; **never synced** |
| Local branch reference `b` | the client's view, `branches` | local `publish` (CAS fast-forward), `fetch` fast-forward, local `branch create`/`delete`/`rename` (ADR 0007 §9) |
| Remote-tracking reference `remote/<r>/<b>` | the client's view, the separate map `remotes` (RFC 0001 §3.4), with the relay's `seq` | only `fetch` and `push` |
| Authoritative published head of `b` | the relay's `state.json` | only the relay's CAS `publish` (§6) |

Local branch names can never start with `remote/` or `remotes/` (ADR 0007
§5), so `remote/<r>/<b>` never shadows a local branch. Adding the first
remote upgrades the repository to format 2 (RFC 0001 §7).

**Rules of `fetch`**, with `L` the local branch, `T` the remote-tracking
reference before the fetch, `R` the relay's head (implemented by
`concurv_workspace::reconcile::plan` and `concurv_sync::fetch`):

0. **Names are checked.** A relay branch name must be a valid new local
   name (Git ref rules, portability policy); otherwise it is ignored
   (`invalid_name`). A new local branch is created only if it collides with
   no existing local name, compared component-wise and case-insensitively;
   among new names, the first in byte order wins (`name_collision` for the
   rest). Remote-tracking references keep the relay's names.
1. **Fetched history is preserved.** Objects are verified and adopted
   (§7), then `T := R`, whatever the relation of `L` and `R`.
2. **Local references move only forward.** If `L` is an ancestor of `R`,
   `L := R` (`fast_forwarded`); if `L` is missing and `T` was missing too
   (a branch new to this repository), `L := R` (`created`). If `L` is
   missing but `T` existed, the branch was deleted or renamed here (ADR
   0007 §9): `L` is not recreated (`not_local`; `branch create <b> --at
   remote/<r>/<b>` brings it back). Otherwise `L` stays (`local_ahead`,
   `diverged`). A workspace integrates only through `update` or `merge`;
   fetch never changes files or workspace heads.
3. **No implicit publication.** `push` offers `publish {expected: R,
   proposed: L}` only if `R` is missing or an ancestor of `L`. If they
   diverged, nothing is published (`CONCURV_DIVERGED`, exit 42): a user or
   agent integrates with `concurv merge remote/<r>/<b>` (or, if `L` is
   still behind, `concurv update` after the fetch fast-forwarded it),
   `concurv publish`, then pushes a merge whose CAS expects `R`. If the
   relay's head is unknown locally, push refuses the same way: fetch first.
4. **Stale and racing publications** are rejected by the relay's CAS
   (`CONCURV_STALE_HEAD`, exit 41, naming the observed head), never
   force-applied. The loser fetches again and rule 3 applies.
5. **A rewound relay is not followed.** If `R` does not descend from `T`
   (the relay was restored from an old backup, or replaced), the branch is
   reported `rewound`, and neither `L` nor `T` changes; `push` to it is
   refused. A branch that vanished from the relay is reported `vanished`
   and keeps `T`.
6. **Both sides stay reachable.** `L`, `T` and every workspace head stay
   references, so no acknowledged snapshot becomes unreachable (ADR 0007
   B1). Fetch and push only add or move references forward. `remote
   remove` drops a remote's tracking references but no local branch.
   (`branch delete` refuses to drop the only reference to a snapshot, ADR
   0007 §9.)

## 6. Publish (relay side)

Under the relay lock (a process mutex plus a file lock on `<relay>/lock`):

1. Authorization: `write` and `publish:<branch>` or `publish:*`.
2. **Request memory**: if `request_id` (32 hex digits, chosen by the
   client) is among the last 1024 successful publishes, return that result
   with `replayed: true` (the same id for a different publish is a protocol
   error). A retry whose first reply was lost is never applied twice.
3. **CAS**: the branch must still be at `expected` (`null`: must not
   exist), else `CONCURV_STALE_HEAD` with the observed head and `seq`.
4. A new branch name must pass the rules of ADR 0007 §5 against the
   relay's existing branches.
5. **Closure**: the proposed snapshot's graph must be complete in the store
   plus this session's quarantine, else `CONCURV_OBJECT_MISSING` with the
   missing ids. The quarantined objects of that graph are adopted into the
   store, referenced objects first.
6. **Forward only**: `proposed` must descend from the current head, else
   `CONCURV_DIVERGED`.
7. `state.json` (heads, `seq`, request memory) is replaced atomically:
   write, fsync, rename, fsync. **That rename is the commit point.** Then a
   line is appended to `publish.log` (audit only, never read back).

`state.json` is the relay's only mutable pointer and only ever names
snapshots whose whole graph is durable in its store. A crash at any point
leaves the old or the new head (tested at every write step, §10).

## 7. Quarantine and graph closure

Received objects (by the client in `fetch`, by the relay in `upload`) are
written into a **temp session** (RFC 0004 §5) holding a private object
store, never directly into `objects/`. Each is checked on receipt: header
before allocation, size limits, id from the bytes, full decoding of trees
and snapshots. Before any reference names them, the graph closure of the
wanted snapshots is checked over quarantine ∪ store, through the typed,
verifying readers (a blob where a tree is expected is
`CONCURV_WRONG_OBJECT_KIND`). Only then are the objects adopted, by durable
rename, in referenced-before-referencing order. If anything is wrong,
nothing is adopted and no reference changes; the session is deleted, or
removed by recovery if the process died.

Store invariant relied on: **an object present in a store has its whole
graph in that store** (`save` writes blobs, then trees bottom-up, then the
snapshot; adoption preserves the order). Walks stop at objects the
receiver already has.

## 8. Retries and idempotency

- Objects are content-addressed: duplicates and reordering are harmless
  (tested: everything uploaded twice, in reverse dependency order).
- `push` retries a lost connection twice (200 ms, 400 ms). If the publish
  request may have been sent, the retry first repeats it with the same
  `request_id`; the relay answers from its request memory, or the normal
  flow runs again.
- `fetch` has no partial state to resume: a failed fetch changes nothing
  and is simply run again. Transfers resume at object granularity only
  (objects already adopted are not sent again).

## 9. Not in version 1

- Several repositories per relay; relay-to-relay replication; relay
  high availability. A relay is one directory; back it up like a
  repository (stop it, copy the directory).
- Resuming inside a partially sent object; pack compression.
- Branch deletion and renaming at the relay (backlog P2-8); protected
  branches (R4, P2-9).
- Key rotation of the relay certificate (re-initialize and re-pin);
  per-device keys instead of bearer tokens; token expiry.
- Awareness events and `--watch` (R5); verified publication (R4).
- Signatures or attestations of snapshots (R7): the relay authenticates
  devices, content addressing gives integrity, not authorship.

## 10. Tests (R3 gate)

`crates/concurv-sync/tests`: `two_machines.rs` (offline divergence and
reconnection in both orders; racing pushes with exactly one winner and
eventual integration of all work; stale CAS, request memory, forward-only,
missing graph; authentication, scopes, revocation; rewound relay;
disconnects at every byte offset of a push and of a fetch, including lost
publish replies), `hostile.rs` (malformed messages and objects to the
relay; wrong certificate with no application data sent; a lying relay
sending forged, incomplete, wrong-kind or oversized objects and invalid or
colliding names), `sync_faults.rs` (an injected failure at every write step
of a relay publish and of a client fetch, as crash and as transient error).
`crates/concurv-cli/tests/sync_cli.rs`: real `concurvd` and `concurv`
processes, every refusal's exit code, and kills of the relay and of
clients in the middle of pushes. `concurv-objects/tests/transfer.rs`: the
view extension's canonical form and receiving hostile objects.
