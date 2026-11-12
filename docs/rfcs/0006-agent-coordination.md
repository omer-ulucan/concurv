# RFC 0006 — Agent coordination (R5)

Status: **Implemented** (2026-11-12) in `concurv_workspace::coord`, the
relay's intent requests, and `concurv intent | overlaps | events`. Gate
(`docs/ROADMAP_REVISED.md`): *versioned JSONL events, advisory intent with
TTL, overlap warnings with measured false-positive/negative rates; no LLM
dependency.* From the original plan: *a cross-PC function signature change
creates a warning to a dependent agent; disconnected agents reconcile
without turning expired intent into a lock; no secrets silently emitted.*

Everything here is **advisory**. No command ever refuses work because of an
intent or a warning; nothing here is a lock or a permission.

## 1. Events (`concurv events`)

One JSON object per line, derived from the operation log (oldest first,
or the last `-n`), schema version 1:

```json
{"concurv_event": 1, "type": "operation", "operation": "<id>", "kind": "save",
 "time_ms": 1793491200000, "device": "<id>", "workspace": "agent-1",
 "branch_moves": {"main": {"from": "<id>|null", "to": "<id>"}},
 "head": "<id>|null", "description": "save: ..."}
```

Additive fields may appear; removing or changing one bumps
`concurv_event`. Events carry ids, names and the descriptions users and
agents wrote; never file contents, tokens or environment variables.

## 2. Intents

An intent says "this workspace (or agent) is about to change these paths
and/or symbols, for a while":

```json
{"concurv_intent": 1, "id": "<32 hex>", "workspace": "agent-1",
 "who": "<author>", "paths": ["src/parser/**"], "symbols": ["parse"],
 "note": "free text", "created_ms": 0, "expires_ms": 0, "origin": "local"}
```

- `concurv intent declare <path-pattern>... [--symbol S]... [--ttl 30m]
  [--note T]`, `intent list`, `intent release [ID]`. TTL 1 minute to
  7 days (default 1 hour). Patterns use the ignore-pattern syntax.
- Stored per repository in `.concurv/intents/<id>.json` (shared by all its
  workspaces, not history). An expired intent is ignored everywhere and
  removed at the next write: it can never act as a lock, including for
  agents that were offline when it expired.
- **Across machines**: `push` sends this repository's active intents to
  the relay (`declare`, scope `write`); `fetch` downloads the relay's
  active intents of other devices (`intents`) into
  `.concurv/remote-intents/<remote>.json`. The relay keeps them in memory
  of its state file and drops expired ones; they are never part of
  history.

## 3. Overlap warnings (`concurv overlaps`, and in `status`)

The **local change set** of a workspace: paths changed between the merge
base with its tracked branch and its working copy (saved and unsaved).
Warnings compare it with:

| Source | Level | Meaning |
|---|---|---|
| another workspace's unpublished saves (same repository) | `path` | both touch the same file |
| same | `conflict` | a three-way merge of the two versions (ADR 0006 rules) would conflict |
| the tracked branch (or its remote-tracking reference) ahead of this workspace | `path` / `conflict` | as above, against work not yet integrated |
| an active intent of another workspace or device | `intent` | a changed path matches a declared pattern, or a changed file mentions a declared symbol |
| a definition changed upstream | `dependency` | upstream work changed the definition line of a symbol (`fn`, `def`, `function`, `class`, `struct`, `enum`, `trait`, `interface`, `type`, `func`) that this workspace's changed files mention |

The `dependency` rule is the "function signature change" warning: when
`fetch` brings a change of `fn parse(` to `fn parse(&self, strict: bool)`
and this workspace's changes call `parse`, the agent is warned before it
publishes. Detection is lexical (no parser, no LLM): definition lines are
recognized by keyword, identifiers by word boundaries.

## 4. Measured rates

`concurv-workspace/tests/overlap_rates.rs` generates a seeded corpus of
paired edits on synthetic source files and compares each prediction with
the actual outcome: the real three-way merge (`textmerge::merge3`) for
`path` and `conflict`, and the constructed dependency for `dependency`. It
prints and asserts the false-positive and false-negative rates; the
numbers are recorded in `bench/RESULTS.md`.

## 5. Not done

Symbol analysis beyond lexical definitions; real-time push of intents and
events (`--watch`); per-agent identities beyond the author string and
workspace name.
