# Changelog

All notable changes to concurv. Releases before 0.1.0 are alphas: format
version 1 and the CLI/JSON contract are not frozen until 0.1.0 (see ADR 0003).

## [Unreleased]

## [0.1.0-alpha.4] — 2026-11-12

Fourth pre-release (`docs/releases/v0.1.0-alpha.4.md`): branch delete and
rename, workspace remove, tags at the relay, comparison harness. Built and
tested by the local CI gate on Windows and Linux; no macOS binary (build
from source). No license is asserted yet.

### Added
- **`concurv branch delete <name>`** and **`concurv branch rename <old>
  <new>`** (backlog P2-8, ADR 0007 §9). Delete refuses tracked, protected
  (exit 44) and unmerged branches (exit 42: work that no other branch,
  tag, workspace or remote-tracking reference contains); only the name
  goes and the head is printed. Rename moves the workspaces tracking the
  branch in the same operation. `undo` refuses both.
- **`concurv workspace remove <name>`** (backlog P1-8): unregisters another
  workspace; its directory and files stay. Refused for the primary or
  current workspace, with unsaved changes (20), a pending merge (43) or
  saves no other reference contains (42). `undo` refuses it.
- Comparison harness (R0): `bench/scenarios/` scripts for concurv, Git and
  jj; concurv and Git results for five scenarios on Windows and Linux in
  `bench/results/`.
- `--at` (`branch create`, `tag create`) also accepts a remote-tracking
  reference, `remote/<remote>/<branch>`.
- **Tags at the relay** (ADR 0008 §7): `concurv push --tags` creates this
  repository's tags at the relay (create-only, all or nothing, for
  snapshots of published history; scope `write`); `fetch` and `clone`
  create the relay's tags that are missing here and never move a local
  tag (conflicts are reported). New protocol request `tags`; relays up to
  v0.1.0-alpha.3 do not answer it, and then there are no tags, as before.

### Changed
- `fetch` no longer recreates a local branch that was deleted or renamed
  here (its remote-tracking reference still follows the relay); the new
  branch action is `not_local`. Branches new to this repository are still
  created.

## [0.1.0-alpha.3] — 2026-11-12

Third pre-release (`docs/releases/v0.1.0-alpha.3.md`): protected branches
(R4), agent coordination (R5), tags, Git import into existing history,
configuration and completions (R6). Built and tested by the local CI gate on
Windows and Linux; no macOS binary (build from source). No license is
asserted yet.

### Added (R6: developer beta, Git ecosystem)
- **Tags** (ADR 0008): `concurv tag create|list|delete`. Tags name a
  snapshot and never move; `--at` (also for `branch create`) accepts a
  branch, a tag or a snapshot id and refuses a name that is ambiguous
  (exit 51); `undo` refuses tag operations; `verify` treats tags as roots.
- **Git tags**: `git export --tags` writes lightweight tags together with
  the branch in one reference transaction (a conflicting Git tag refuses
  the export); `git import --tags` imports the tags of the imported
  history (annotated tags become lightweight).
- **Import into existing history**: `git import` reuses commits concurv
  already has (from earlier imports and exports) and imports only new ones;
  `--into C` creates or fast-forwards another branch; a diverged target is
  refused (exit 42). Round trips Git → concurv → Git keep the Git commit
  ids. The workspace is checked out only if that cannot lose anything.
- **Configuration files**: `concurv config get|set|unset|list` with keys
  `author` and `remote`, in `.concurv/config.json` (per repository) or the
  user file (`--global`; `$CONCURV_CONFIG`, `%APPDATA%`,
  `$XDG_CONFIG_HOME`). Malformed files and unknown keys are errors.
- **Shell completions**: `concurv completions bash|zsh|fish|powershell|elvish`.
- `CONTRIBUTING.md`, `SECURITY.md` (private vulnerability reporting),
  `CODE_OF_CONDUCT.md`, issue and pull request templates.

### Changed (format, R6)
- **Repository format 4**: operations may carry tags (a second view
  extension, RFC 0001 §3.4); upgraded by the first tag. Builds before R6
  refuse it.
- `git import` into a repository that already has `main` no longer refuses
  outright: it imports on top if `main` is contained in the Git branch.
- `op log --json` has a `tags` object per operation.

### Added (R5, RFC 0006: agent coordination)
- **Advisory intents**: `concurv intent declare|list|release` (paths and
  symbols, 1 minute to 7 days); exchanged with the relay on `push` and
  `fetch`; expired intents are ignored everywhere. Never a lock.
- **`concurv overlaps`**: warnings about the same files changed elsewhere,
  line edits that would conflict, declared intents, and upstream changes to
  the definition of a function or type this workspace uses (a signature
  change on another machine warns after `fetch`). Measured rates in
  `bench/RESULTS.md`.
- **`concurv events`**: the operation log as versioned JSON lines.

### Added (R4, RFC 0005: verified integration)
- **Protected branches.** A `concurv-policy.json` in a branch's current head
  can `protect` it: `publish` (and, at a relay, `push`) then needs a passing
  attestation for exactly the new head, under exactly that policy version.
  A candidate cannot weaken its own policy; two changes that pass apart and
  fail together cannot be published.
- **`concurv check [--branch B] [--trust]`**: runs the policy's checks on a
  fresh checkout of the saved snapshot (never the working copy), with a
  cleared environment and timeouts, and records an attestation (new object
  kind 6). Runs only policies approved on the device (`--trust`).
  `publish --check` checks first.
- Relay: scope `attest` and the `attest` request; a protected branch at the
  relay accepts only attestations vouched for by `attest` devices.
- Exit codes 44 (`CONCURV_POLICY_FAIL`) and 45 (`CONCURV_UNTRUSTED_POLICY`).

### Changed (format, R4)
- **Repository format 3**: the store may contain attestation objects;
  upgraded by the first `concurv check`. Builds before R4 refuse it.

## [0.1.0-alpha.2] — 2026-11-12

Second pre-release (`docs/releases/v0.1.0-alpha.2.md`): named branches and sync
between machines. Built and tested by the local CI gate on Windows and Linux;
no macOS binary (build from source). No license is asserted yet.

### Added (R3, RFC 0003: sync between machines)
- **`concurvd`**, the relay: one directory per repository, the single
  authority for published branch heads; `init`, `fingerprint`, `device
  add|list|revoke`, `serve`. TLS 1.3 with a self-signed certificate that
  clients pin by fingerprint; per-device tokens with scopes `read`, `write`,
  `publish:<branch>|*`.
- **`concurv remote add|list|remove`, `fetch`, `push`, `clone`.** Fetch
  records the relay's heads as remote-tracking references
  `remote/<remote>/<branch>` and fast-forwards local branches that are
  behind; push is a compare-and-swap at the relay, fast-forward only, with
  request ids so a retried push never publishes twice. Diverged work is
  integrated with `concurv merge remote/<remote>/<branch>`.
- Everything received is quarantined, verified object by object and
  checked for graph closure before anything references it; a relay whose
  head went backwards is reported and never followed.
- Exit codes 70–75: network, protocol, protocol mismatch, authentication,
  permission, relay identity.

### Changed (format, R3)
- **Repository format 2**: operations may carry remote-tracking references
  (an optional view extension; existing operations keep their bytes and
  ids). A repository is upgraded when its first remote is added; older
  builds refuse it with exit 12 (ADR 0001 migration note).
- Release archives contain both `concurv` and `concurvd`.

### Added (R2.5, ADR 0007)
- **Named branches**: `branch create [--at | --switch]`, `branch list`,
  `branch switch`; `workspace add --branch`; `git export --from <branch>`.
- **`concurv merge <branch>`** integrates another branch (same rules as
  `update`; publishes nothing). `update` integrates only the tracked branch.
- `branch create --switch` keeps unpublished saves on a new branch in one
  operation, without touching files.
- **Transactional `branch switch`**: refuses unsaved changes, colliding
  untracked or ignored files, pending merges and saves that would be left
  on no branch; keeps non-colliding untracked files; rolls back on I/O
  failure; an interrupted switch is finished by running it again (`status`
  reports it). `undo` reverses a switch and a merge.
- Branch names: Git's ref-name rules exactly (tested against
  `git check-ref-format`), a portability policy (ASCII, Windows-safe,
  reserved `refs`/`remote`/`remotes`), and namespace-collision checks
  (case, `x` vs `x/y`).
- R3 groundwork: `reconcile::plan` encodes the per-branch reconciliation
  rules of RFC 0003 §5 (not wired to any command).
- New operation kinds `branch-create`, `branch-switch`, `merge`.

### Changed (R2.5)
- A path whose current content already equals the checkout target is no
  longer counted as unsaved work by restore/update/checkout (nothing is
  lost by leaving it); this makes interrupted checkouts resumable.
- The `update --from` option of an unreleased build is replaced by `merge`.

### Changed (project)
- The CI gate runs locally (`scripts/ci-local.sh`, `scripts/ci-all.sh`:
  Windows and WSL); the GitHub workflows start only manually for now.
- The kill-recovery test calibrates its kill moments to the machine, so it
  kills processes mid-command on fast machines too.

## [0.1.0-alpha.1] — 2026-11-01

First public pre-release (`docs/releases/v0.1.0-alpha.1.md`). No license is
asserted yet.

R1 gate passed on 2026-11-01: the full suite is green on Linux, macOS and
Windows in CI, with identical golden hashes on all three (`docs/ROADMAP_STATUS.md`).
R2 slices 2a (private workspaces), 2b (undo), 2c (conflict resolution) and 2d
(Git bridge) are implemented, and the **R2 gate passed** on 2026-11-01 (CI on
`main`, Linux/macOS/Windows; Git tags are out of scope, see `docs/ROADMAP_STATUS.md`).

### Changed (behaviour)
- **`save` no longer moves `main`** (RFC 0004 §2). It advances only the
  current workspace's head; use `concurv publish` to move `main`.
- Mutating commands **wait** up to 30 s for a busy lock instead of failing
  immediately with `CONCURV_LOCKED`.
- The repository lock is now held only for the commit step; scanning and
  object writing run concurrently with other workspaces.

### Changed (format, pre-release)
- Operation views store a **name** for every workspace (RFC 0001 §3.4,
  RFC 0004 §4). Repositories created by earlier builds of this branch are
  not readable; none were distributed.

### Added (R2 slice 2a, RFC 0004)
- `concurv workspace add <path> [--name N]` and `concurv workspace list`:
  private workspaces that share one store and one `main`.
- `concurv publish`: compare-and-swap fast-forward of `main`;
  `CONCURV_DIVERGED` (exit 42) if `main` has unintegrated work.
- `concurv update`: fast-forward, or a file-level three-way merge into a new
  snapshot in the workspace. Same-file conflicts and ambiguous merge bases
  are refused with `CONCURV_CONFLICT` (exit 43); nothing is modified.
- Stress tests: 2/5/10 concurrent writers; 12 rounds of killed processes.

### Added (R1.5, ADR 0004)
- **Ignore rules like Git**: `.gitignore` (root and nested, negation, anchors,
  `**`, escapes) and a local `.concurv/exclude`. Already-tracked files stay
  tracked. Verified against real Git by a differential test.
- `concurv check-ignore <path>…` explains decisions (`--json`), including
  unusable patterns.
- **Stat cache** (ADR 0005, Unix only): `status`/`diff` skip rehashing files
  whose exact identity (device, inode, size, mode, mtime, ctime) is unchanged
  since the last save; a racy-timestamp guard and a false-clean test suite.
  `status --no-cache` / `diff --no-cache` hash every file. Windows always
  hashes.
- `concurv-bench`: reproducible `status`/`save` benchmark (`bench/RESULTS.md`).
- Golden tests for diff output (` No newline at end of file`, CRLF, binary)
  and a case-collision restore test that runs on every OS.

### Added (R2.5, ADR 0006)
- **Line-level merge in `update`**: text files changed on both sides merge
  when their edits are separated by at least one unchanged line
  (conservative diff3; CRLF and missing final newline kept byte-exactly).
  Binary files, symlinks and overlapping or touching edits stay conflicts.
  `update --json` lists them in `content_merged[]`.

### Changed (R1.5)
- `.git/` directories are never scanned, and untracked nested repositories
  are skipped. History saved by earlier builds that captured `.git/` files
  shows them as deleted.
- `restore`, `update`, `undo` and workspace checkout refuse to overwrite an
  ignored, untracked file, even with `--force` (exit 20), and refuse to write
  a `.git` entry (exit 22).
- License metadata removed: no license is asserted until the owner chooses one.
- `save` hashes a file before storing it and skips the temp write and fsync
  when a verified copy of the object already exists: a no-op save of 10k
  files went from 46.0 s to 1.7 s on the reference machine.

### Added (R2 slice 2d, ADR 0002)
- `concurv git export <repo> [--branch B]` writes `main`'s history (merges
  included) into a Git repository via `git fast-import` and moves the Git
  branch by compare-and-swap; refuses (exit 42) if the Git branch has
  commits outside that history. Deterministic and incremental.
- `concurv git import <repo> [--branch B]` imports a Git branch into a fresh
  repository, publishes it as `main` and checks it out; refuses submodules,
  `.concurv` paths and non-UTF-8 data with a full list (exit 23).
- New crate `concurv-git`; new error code `CONCURV_EXTERNAL_TOOL` (exit 60).

### Added (R2 slice 2c, RFC 0004 §11)
- `concurv update --allow-conflicts` records a pending merge instead of
  refusing: clean changes are applied, conflicting text files get whole-file
  ours/base/theirs markers, binary and delete/modify conflicts keep ours with
  theirs in `<path>.concurv-theirs`, and every conflict gets a
  `<path>.concurv-conflict` note. `save` completes the merge once nothing
  remains (both parents); `update --abort` gives it up (edits backed up).
  `status` shows each conflict as resolved/unresolved; `undo` works.
- Format (pre-release, within v1): conflict object kind 5; workspace view
  entries can reference a pending merge.

### Added (R2 slice 2b, RFC 0004 §10)
- `concurv undo` reverses the most recent operation of the current workspace
  (`save`, `update`, `restore`), exactly what it changed; repeat to go further
  back. Never touches other workspaces or branches; `publish` is refused.
  Undoing `restore --force` brings unsaved work back exactly.
- Operation format (pre-release, within v1): `undoes` and `checked_out`
  fields, also shown in `op log --json`.

### Fixed (found by the new stress tests before release)
- Data integrity: the primary workspace's temp cleanup could delete other
  writers' in-flight temp files, which on Windows could leave `op_head`
  pointing at a vanished operation object. Workspaces now use a separate
  `ws-tmp/`, and each temp root is cleaned only under its own lock.
- Cleanup races with an exiting writer no longer fail commands (best effort).

### Added
- `concurv op log [-n N]` lists the operation log (init/save/restore), newest
  first, with the head after each operation and any backup snapshot recorded
  by `restore --force`. A lost backup id can now be found again.
- GitHub Actions CI: fmt, Clippy, tests and release build on Linux, macOS
  and Windows with the latest stable Rust, plus an MSRV check on Rust 1.89.

### Fixed
- The save/init fault-injection sweeps were flaky (about 1 run in 5): the
  number of directory creations depends on random ids, so a precomputed hit
  count could overshoot. Each case now discovers its own boundaries.
- The non-UTF-8 file-name test now accounts for macOS, whose APFS refuses
  such names at creation (`EILSEQ`).
- Code now passes the Clippy lints added in Rust 1.99 (`as_chunks` for hex
  decoding, a byte-string literal in a test). No behaviour change.

### Changed
- **Project renamed from ARC to concurv** (ADR 0003). Binary `arc` → `concurv`,
  crates `arc-*` → `concurv-*`, metadata directory `.arc/` → `.concurv/`,
  object magic `ARC1` → `CNV1`, hash context → `concurv 2026-11 object v1`,
  error codes `ARC_*` → `CONCURV_*`, JSON field `arc_json_version` →
  `concurv_json_version`, env `ARC_AUTHOR` → `CONCURV_AUTHOR`. Every object id
  changed. Repositories from pre-rename builds are not readable and no
  migration is provided, because none were distributed.

### Added (R1 local kernel)
- `concurv init` creates the repository atomically (staging directory + rename).
- `concurv status` / `concurv diff` are read-only, content-hash based comparisons
  of the working copy with the head.
- `concurv save -m` writes a durable snapshot. Objects are fsynced and renamed,
  then a single atomic swap of `.concurv/op_head` commits the operation.
- `concurv log` shows first-parent snapshot history.
- `concurv restore --snapshot <id> [--force]` verifies the target fully first,
  refuses to discard unsaved work without `--force`, and saves a backup
  snapshot when forced. It never writes through symlinks, and an interrupted
  restore is reported and can be resumed.
- `concurv verify [--full]` checks the integrity of the object graph.
- `--json` output (`concurv_json_version` 1) with stable error and exit codes.
- Canonical BLAKE3 object format (RFC 0001), durability protocol (RFC 0002),
  sync design (RFC 0003, not implemented), threat model.
- 59 tests: golden vectors, property tests, a fault-injection sweep across
  every durable-write boundary, and restore-safety and CLI smoke tests.

### Known gaps
- No stat cache, GC, rename detection, line-level (within-file)
  merging, cross-machine sync, importing Git changes into existing history,
  or Git tags.
