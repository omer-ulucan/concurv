# RFC 0005 — Verified integration (R4 design; NOT IMPLEMENTED)

Status: **Draft.** No code implements this RFC yet. It specifies the R4 gate
of `docs/ROADMAP_REVISED.md`: *candidate hash + policy + environment-bound
attestation; stale attestation rejected; failing merged test blocks
publish; no TOCTOU; sandbox policy explicit.*

## 1. Problem

A clean textual merge is not a working program. Today `publish` (local) and
`push` (relay) move a branch to any snapshot that descends from its head.
R4 lets a branch require **evidence** that the exact snapshot about to be
published passed the branch's checks, without trusting the working copy,
without letting a candidate weaken its own checks, and without running
repository code that nobody approved.

## 2. Terms

- **Candidate**: the snapshot proposed as a branch's new head (usually the
  workspace head after `update`/`merge`).
- **Policy**: a versioned file `concurv-policy.json` at the root of the
  tree. The policy that applies to a publication is the one in the
  **branch's current head**, never the candidate's: a candidate cannot
  change the rules it is judged by. A policy change takes effect once it is
  itself published (under the old policy).
- **Check**: a command (argv, no shell) the policy declares, with a timeout.
- **Attestation**: an immutable object recording that the checks of one
  policy ran on one candidate in one environment, and their results.
- **Protected branch**: a branch whose policy requires a passing
  attestation for every publication.

## 3. Policy file (versioned)

```json
{
  "concurv_policy": 1,
  "protect": ["main"],
  "environment": { "os": ["linux", "windows"] },
  "checks": [
    { "name": "test", "argv": ["cargo", "test", "--locked"], "timeout_s": 1800 }
  ],
  "sandbox": "none"
}
```

- Unknown fields or versions: the policy is invalid and the branch is
  treated as protected with no way to satisfy it (fail closed), with a
  clear error.
- `sandbox` must be stated. Version 1 knows only `"none"`: checks run as
  the user, without isolation, in a fresh directory (§5). Future adapters
  (container, VM) get new values; a value this build does not know is
  refused.

## 4. Attestation object (new object kind 6, format change)

```
Id      candidate        (snapshot)
Id      policy           (blob id of concurv-policy.json in the branch head)
String  branch
Rid     device
i64     started_ms, finished_ms
String  environment      (canonical JSON: os, arch, concurv version, and
                          the output digests of the policy's probes)
u32     check_count; repeat (in policy order):
  String name
  u32    argc; repeat String arg
  i32    exit_code       (or a reserved value for timeout / not started)
  u64    duration_ms
  Id     output_digest   (BLAKE3 of stdout+stderr, stored as a blob)
u8      result           (1 = pass: every check exited 0)
```

Content-addressed like every object; old decoders reject kind 6 with
`CONCURV_UNSUPPORTED_FORMAT` (RFC 0001 §7). Attestations are local unless
pushed (§7).

## 5. Running checks: `concurv check`

1. Resolve the candidate (default: this workspace's head) and the target
   branch (default: the tracked branch); load the policy from the
   branch's head (none: nothing to check).
2. **Trust**: refuse unless this policy blob id was approved on this
   device (`concurv check --trust`, recorded in `.concurv/trusted-policies`,
   local). Running repository code is opt-in per policy version (threat
   T12).
3. **No TOCTOU**: materialize the candidate's tree from the store into a
   fresh temp directory (never the working copy), verifying every object.
   The checks see exactly the candidate's bytes.
4. Run each check with a cleared environment (allow-list: `PATH`, `HOME`,
   `USERPROFILE`, `SYSTEMROOT`, `TEMP`/`TMPDIR`, plus names the policy
   lists), the temp directory as working directory, and its timeout.
5. Write the attestation (pass or fail) and print its id. Nothing is
   published.

## 6. Publishing to a protected branch

`publish` (local) to a protected branch requires an attestation `A` with:
`A.candidate` = the workspace head; `A.policy` = the policy blob id in the
branch's **current** head (a newer policy makes `A` stale); `A.branch` = the
branch; `A.result` = pass; the environment allowed by the policy. Otherwise
it is refused (new exit code), naming what is stale or failing. `publish
--check` runs `concurv check` first. A merge whose tests fail therefore
cannot be published to a protected branch.

## 7. Relay

- Protection is configured at the relay (`concurvd branch protect`), which
  also pins the policy rules it accepts.
- New scope `attest`. A device with it submits attestations
  (`attest {id}` request, after uploading the object); the relay records
  which device submitted each. A `publish` to a protected branch must name
  an attestation that satisfies §6 against the relay's head and was
  submitted by an `attest` device. The relay checks the binding, not the
  claims: authenticity comes from device authentication, until signatures
  (R7).

## 8. Gate tests (to write)

Stale attestation (other candidate, older policy, other branch) refused;
failing check blocks publish locally and at the relay; the candidate
cannot change its own policy; a working-copy edit after `check` does not
change what was checked; an untrusted policy never runs; unknown sandbox
refused; attestations from devices without `attest` refused; crash
injection during check and publish.

## 9. Open questions

- Environment binding strength: OS/arch and probe digests are claims of
  the runner; signatures and hardware attestation are R7.
- Caching attestations across equivalent candidates (same tree, other
  parents)?
- Policy for branches created at the relay (inherit from the parent?).
