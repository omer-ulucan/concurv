<!-- Contributions are dual licensed MIT OR Apache-2.0 (see CONTRIBUTING.md). For larger changes, please open an issue first. -->

**What and why**


**How it is tested** (new tests, failure paths covered, local gate run)


**Checklist**

- [ ] `cargo fmt`, `cargo clippy -- -D warnings` (also for a Unix target from Windows), `cargo test` pass
- [ ] No change to the byte format, or a new format version with an upgrade path (RFC 0001)
- [ ] `docs/cli.md`, the user guide and `CHANGELOG.md` updated if behaviour changed
- [ ] What is not covered or not proven is stated
