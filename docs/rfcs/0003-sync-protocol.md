# RFC 0003 — Sync protocol (R3 design; NOT IMPLEMENTED)

Status: **Draft specification only.** No code in this repository implements
any part of this RFC. It exists so that the R1 storage format does not
preclude a safe R3.

## 1. Roles

- **Client** — an concurv repository on a device (PC A, PC B, …).
- **Relay (`concurvd`)** — a small self-hostable server that stores objects and is
  the **single authority** for *published* branch heads of a repository. It is
  a coordinator, not a P2P consensus participant. There is exactly one
  authoritative relay per published branch.

Properties we keep: offline work is always possible; offline heads may
diverge; only the relay's CAS decides the published head; nothing a peer sends
is executed or trusted without verification.

## 2. Separation of visibility levels

| Level | Effect | Mutates working files? |
|---|---|---|
| `awareness` | ephemeral presence/intent events (R5) | never |
| `fetch` | download missing objects + remote head pointers into `remote/<relay>/<branch>` | never |
| `apply` | explicit user/policy action integrating fetched changes into a private workspace | yes, via normal restore safety rules |
| `publish` | CAS of the relay's branch head | never locally |

## 3. Session

1. **Transport + authentication** — TLS (rustls). Device authenticates with a
   per-device key or scoped token; the relay maps it to a repository and a set
   of scopes (`read`, `write-objects`, `publish:<branch>`).
2. **Hello** — both sides send `{protocol: "concurv-sync", versions: [1], object_format: 1, capabilities: [...]}`;
   the highest common version is used; no common version → `CONCURV_PROTOCOL_MISMATCH`.
3. **Head advertisement** — relay sends published heads `{branch → (snapshot_id, publish_seq)}`.
4. **Negotiation** — client sends wants/haves in bounded batches (ids only);
   relay answers with the missing-set. Commit-graph walking stops at common
   ancestors; batches are capped (e.g. 4096 ids, 1 MiB per message).
5. **Transfer** — objects stream in chunks with per-chunk BLAKE3 checksums and
   an overall byte budget. Transfers are resumable by `(transfer_id, offset)`.
6. **Quarantine** — received objects land in `quarantine/incoming/<transfer>`
   on the receiver. Before promotion each object is re-hashed, decoded with
   the R1 limits, and **graph closure** is checked (every referenced object is
   present locally or in the same quarantine). Only then are objects moved into
   `objects/` using RFC 0002 §3.1.
7. **Metadata replication** — operation and change records are idempotent
   (content-addressed): re-receiving an existing id is a no-op (I10).
8. **Publish** — `Publish{branch, expected_head, proposed_head, attestation?}`.
   The relay, under a per-branch lock: checks authorization; checks the
   proposed snapshot and its whole graph are present and verified; checks
   policy (R4: attestation bound to `proposed_head` + policy hash); then
   compares `expected_head` with the current head. Equal → durably write the
   new head, increment `publish_seq`, acknowledge. Different →
   `CONCURV_STALE_HEAD{observed_head, publish_seq}` and nothing changes.

## 4. Idempotency and replay

- All object and operation messages are content-addressed; duplicates,
  reordering and delays are harmless.
- `Publish` carries a client-generated `request_id`; the relay remembers recent
  results so a retried publish whose ack was lost returns the original result
  instead of being re-evaluated (prevents double-publish confusion).
- Events carry `(device_id, monotonic seq)` and a relay timestamp; stale or
  replayed awareness events are dropped after their TTL.

## 5. Offline divergence

Clients record the last fetched published head in their View as
`remote/<relay>/<branch>`. A local branch can advance while offline. On
reconnect: fetch → the client sees `local` and `remote` heads; if `local` does
not descend from `remote`, publishing requires integration (R4) producing a
new candidate whose parents include the remote head. A stale publish is
rejected, never force-applied. Nothing is overwritten: both heads remain
reachable on the client.

## 6. Security requirements (see threat model)

Bounded message sizes, decompression ratio limits, graph-walk budgets,
authorization on every request, no execution of received content, no
automatic transfer of private workspaces, backup snapshots, or provenance
metadata (they are local-only unless explicitly published).

## 7. Open questions (to resolve before R3)

- HTTP/2 vs QUIC transport; long-poll vs WebSocket for `--watch`.
- Pack format for bulk transfer (must not change object ids).
- Relay storage durability (SQLite WAL vs file store reusing `concurv-objects`).
- Key rotation and device revocation flows.
