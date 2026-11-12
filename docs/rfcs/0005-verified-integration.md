# RFC 0005 — Verified integration (R4)

Status: **Implemented** (2026-11-12): `concurv_workspace::{policy, check}`,
the protected `publish`, `concurv check`, `publish --check`, and the relay's
`attest` request and protected `publish` (`concurv-sync`). It covers the R4
gate of `docs/ROADMAP_REVISED.md`: *candidate hash + policy +
environment-bound attestation; stale attestation rejected; failing merged
test blocks publish; no TOCTOU; sandbox policy explicit.*

## 1. Problem

A clean textual merge is not a working program. A branch can require
**evidence** that the exact snapshot about to become its head passed the
branch's checks, without trusting the working copy, without letting a
candidate weaken its own checks, and without running repository code that
nobody approved.

## 2. Terms

- **Candidate**: the snapshot proposed as a branch's new head (the
  workspace head for `publish`, the local branch head for `push`).
- **Policy**: the file `concurv-policy.json` at the root of the
  **branch's current head**, never of the candidate: a candidate cannot
  change the rules it is judged by. A policy change takes effect once it is
  itself published, under the old policy. A branch with no head has no
  policy.
- **Check**: a command (argv, no shell) with a timeout.
- **Attestation**: an immutable object recording that one policy's checks
  ran on one candidate in one environment, with their results.
- **Protected branch**: a branch its governing policy lists in `protect`.

## 3. Policy file (versioned, version 1)

```json
{
  "concurv_policy": 1,
  "protect": ["main"],
  "environment": { "os": ["linux", "windows"] },
  "checks": [ { "name": "test", "argv": ["cargo", "test", "--locked"], "timeout_s": 1800 } ],
  "env": ["CARGO_HOME"],
  "sandbox": "none"
}
```

Strict: unknown fields, a version other than 1, a sandbox other than
`"none"`, 0 or more than 64 checks, duplicate or empty names, empty argv,
more than 256 arguments, timeouts outside 1 s–1 day, invalid branch or
variable names, or more than 64 KiB are errors (`CONCURV_POLICY_FAIL`,
exit 44). An invalid policy in a branch head makes publishing to that
branch impossible until a valid one is published by other means (fail
closed). `environment.os` uses Rust's `std::env::consts::OS` names; empty
allows any.

`"sandbox": "none"` is the only value version 1 knows, and it must be
written out: checks run as the user, **without isolation**, in a fresh
directory, with a cleared environment. Isolating adapters (container, VM)
would be new values that older builds refuse.

## 4. Attestation object (object kind 6; repository format 3)

```
Id      candidate        (snapshot)
Id      policy           (blob id of the governing concurv-policy.json)
String  branch
Rid     device           (the repository's device id)
i64     started_ms, finished_ms
String  environment      (JSON: os, arch, concurv version)
u32     check_count (≤ 64); repeat, in policy order:
  String name
  u32    argc (≤ 256); repeat String arg
  i64    exit_code       (-1000001: timeout, -1000002: could not start)
  u64    duration_ms
  Id     output          (blob: combined stdout and stderr)
u8      result           (1 = pass: at least one check, all exited 0)
```

The decoder rejects an attestation whose `result` disagrees with its exit
codes. Writing the first attestation upgrades the repository to **format
3** (RFC 0001 §7). Attestations are found through a local cache index,
`.concurv/attestations/<candidate>`; every entry is re-read and verified
before use, so a damaged index can only hide attestations, never forge one.

## 5. `concurv check [--branch B] [--trust]`

1. Candidate: this workspace's head; branch: the tracked one (or `B`);
   policy: from the branch's current head (none: `CONCURV_NOT_FOUND`).
2. **Trust**: refused with `CONCURV_UNTRUSTED_POLICY` (exit 45), listing
   the commands, unless this exact policy blob id was approved on this
   device (`--trust` approves it; list in `.concurv/trusted-policies`,
   local). A new policy version needs a new approval.
3. **Environment**: refused (exit 44) if the policy does not allow this OS.
4. **No TOCTOU**: the candidate's tree is written from the store into a
   fresh temp directory, every object verified. The working copy is never
   used; what is checked is exactly what would be published.
5. Each check runs with a cleared environment (allow-list: `PATH`, `HOME`,
   `USER`, `USERNAME`, `USERPROFILE`, `LANG`, `TMPDIR`, `TEMP`, `TMP`,
   `SYSTEMROOT`, `WINDIR`, `COMSPEC`, `PATHEXT`, `APPDATA`, `LOCALAPPDATA`,
   plus the policy's `env`), stdin closed, and its timeout.
6. The attestation (pass or fail) is written and indexed. Nothing is
   published.

## 6. Publishing to a protected branch

Under the repository lock, `publish` reads the policy of the branch's
**current** head. If it protects the branch, it needs a recorded
attestation `A` with: `A.candidate` = the workspace head; `A.branch` = the
branch; `A.policy` = the governing policy's blob id (an older or newer
policy makes `A` stale); `A`'s checks = the policy's checks (names and
argv, in order); `A`'s OS allowed by the policy; `A` passed. Otherwise
`CONCURV_POLICY_FAIL` (exit 44), listing why each recorded attestation
does not qualify; nothing is published. The operation records the
attestation used. `publish --check` runs `check` first.

## 7. Relay

The relay applies the same rule with the policy in **its** current head.
New scope `attest`. A device with it uploads an attestation object and
sends `attest {id}`; the relay decodes it and records, atomically with its
state, which device vouched for it (last 4096). A `publish` to a protected
branch must carry `attestation`: one that an `attest` device vouched for
and that satisfies §6 for exactly the proposed head. `push` does this
automatically when the relay's head (known locally) carries a protecting
policy. The relay checks bindings, not claims: authenticity comes from
device authentication until signatures (R7).

## 8. Tests (R4 gate)

`concurv-workspace/tests/verified_integration.rs`: a failing merged test
blocks publication while each side alone passes; stale attestations (other
candidate, other policy version, other branch) and forged ones (result
contradicting exit codes) are refused; a candidate cannot weaken its own
policy; checks see the snapshot, not the working copy (an ignored file on
disk does not help); untrusted policies never run (side effect absent;
nothing written); timeouts, missing programs, unknown sandboxes and
foreign-OS policies fail closed; attestations record their environment.
`policy.rs` unit tests: invalid policies. `concurv-sync/tests/protected.rs`:
at the relay, push with a vouched passing attestation succeeds; devices
without `attest` cannot vouch; missing, unvouched, failing and
other-candidate attestations are refused.

## 9. Limits and open questions

- Environment binding is a claim of the runner (OS, architecture, concurv
  version); signatures and hardware attestation are R7.
- No isolation (`sandbox: none`); container adapters are future work.
- Attestations are per candidate snapshot; equivalent candidates (same
  tree, other parents) need their own run.
- `fetch` fast-forwards a protected local branch to what the relay
  published; the relay's rule is what protects published history there.
