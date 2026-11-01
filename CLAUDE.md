# concurv — notes for Claude Code sessions

concurv is a Rust, local-first VCS. Product spec: `docs/ARC_Research_and_Roadmap_2026.md`.
The product spec and `prompts/START_WITH_CLAUDE_CODE.md` predate the rename: "ARC"/`arc` there means concurv.
Execution rules: `prompts/START_WITH_CLAUDE_CODE.md`. Current stage and evidence:
`docs/ROADMAP_STATUS.md` — read it first.

Before changing storage, refs, restore or the CLI contract, read:
- `docs/rfcs/0001-vcs-semantics.md` — byte format (normative; changing it needs a new format version)
- `docs/rfcs/0002-durability-and-transactions.md` — write ordering, fsync, commit point
- `docs/security/threat-model.md`
- `docs/adr/0003-project-rename-concurv.md` — identifier names are part of the format; do not change them casually
- `docs/cli.md` — JSON schema, error codes, exit codes (stable contract)

Hard rules:
- Never lose acknowledged data. `.concurv/op_head` is the only mutable pointer and must only
  ever name durable, verified objects. Objects are immutable; never rewrite or delete them
  (no GC exists yet).
- `restore` must never overwrite dirty working files without `--force` + backup snapshot.
- Do not mark a roadmap stage passed without its gate's evidence. Do not add CLI commands
  that are not implemented.
- No `unsafe`, no `unwrap`/`expect` on runtime paths, no ignored tests.
- Work on a feature/fix branch, commit in small steps, merge into `main` with `--no-ff`.

Checks (all must pass before merging):
```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release
```
