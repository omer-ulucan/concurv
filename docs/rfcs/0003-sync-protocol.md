# RFC 0003 — Sync protocol (R3 design; NOT IMPLEMENTED)

Status: **Draft specification only.** No code in this repository implements
any part of this RFC. It exists so that the R1 storage format does not
preclude a safe R3.

## 1. Roles

- **Client** — a concurv repository on a device (PC A, PC B, …).
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
| `fetch` | download missing objects + remote heads into remote-tracking references `remote/<relay>/<branch>` (§5) | never |
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

## 5. Branch references and offline divergence

Revised for the branch model (ADR 0007 §7–8). Reconciliation is per branch.

**Four kinds of state** take part, and only the relay's state is
authoritative across machines:

| Kind | Stored | Moved by |
|---|---|---|
| Workspace state (files, workspace head, pending merge) | the workspace | local commands in that workspace; **never synced** |
| Local branch reference `b` | the client's view, `branches` | local `publish` (CAS fast-forward), the fast-forward below |
| Remote-tracking reference `remote/<relay>/<b>` | the client's view, a **separate** map (never `branches`), plus the relay's `publish_seq` | only `fetch` |
| Authoritative published head of `b` | the relay | only the relay's CAS `Publish` (§3.8) |

The names `remote/…` and `remotes/…` are reserved, so local branches can
never shadow a remote-tracking reference (ADR 0007 §5). Adding the
remote-tracking map to the view is a format change scheduled with R3.

**Rules after a fetch**, with `L` the local reference, `T` the remote-tracking
reference before the fetch, and `R` the relay's head:

1. **Fetched history is preserved.** Objects are verified and promoted
   (§3.6), then `T := R`, whatever the relation of `L` and `R`.
2. **Local references move only forward.** If `L` is an ancestor of `R`,
   `L` may fast-forward to `R`. A workspace tracking `b` still integrates
   only through `update` (its files are never changed by sync, §2).
   Otherwise `L` stays where it is.
3. **No implicit publication.** If `R` is an ancestor of `L`, the client
   may offer `Publish{expected_head: R, proposed_head: L}`. If they diverge,
   nothing is published. A user or agent must integrate `R` with
   `concurv merge` in a workspace, which produces a merge candidate with
   parents `L` and `R`. The candidate must pass the conflict checks and the
   branch's R4 gates. It is then published by a CAS that expects `R`. A
   merge candidate never becomes a published head automatically.
4. **Stale and racing publications** are rejected by the relay's CAS
   (`CONCURV_STALE_HEAD`), never force-applied. The loser fetches again,
   and rule 3 applies.
5. **A rewound relay is not followed.** If `R` does not descend from `T`
   (the relay's head moved backwards or sideways, or the branch vanished),
   the client reports it and changes no reference automatically.
6. **Both sides stay reachable.** On the client, `L`, `T` and every
   workspace head remain references, so no acknowledged snapshot becomes
   unreachable (ADR 0007 invariant B1).

These rules are implemented as a pure decision function,
`concurv_workspace::reconcile::plan`, which is not wired to any command.
They are tested with an in-memory relay model (`tests/reconcile.rs`):
divergence preserved, exactly one winner of racing CAS publications, and a
rewound relay ignored.

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
