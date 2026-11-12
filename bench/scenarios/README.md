# Comparison scenarios (R0)

Reproducible scripts for the scenarios of
`docs/benchmarks/comparison-plan.md`, one per tool:
`S<id>-<name>/<tool>.sh`. Each runs in a fresh temporary directory, uses no
user configuration (`lib.sh` makes Git, jj and concurv hermetic), and
prints one line `RESULT: <pass|partial|fail|unsupported> <detail>`.

```console
$ cargo build --release -p concurv-cli
$ bench/scenarios/run.sh                     # writes bench/results/<date>-<os>.md
$ TOOLS="git jj" bench/scenarios/run.sh      # a subset of tools
$ CONCURV=/path/to/concurv JJ=/path/to/jj bench/scenarios/run.sh
```

A tool that is not installed is reported as `unsupported`. Results record
the OS, the filesystem of the temporary directory (set `TMPDIR` to test
another), the CPU, the concurv commit and every tool's version; no host or
user name.

| Scenario | Question |
|---|---|
| S04 scoped undo | A and B save alternately in their own workspaces; A undoes. Is only A's last operation reverted? |
| S05 CAS publish | A and B publish from the same head. Does exactly one win, without losing the other's work? |
| S09 restore over unsaved edits | Going back to an older snapshot over an unsaved edit: refused, or recoverable? |
| S10 same size, same mtime | A same-size rewrite with the old modification time put back: is it seen? (cf. jj #10509) |
| S12 disjoint hunks | Edits at the top and bottom of one file merge; an edit of the same line is not resolved silently? |

The `jj.sh` scripts were written for Jujutsu 0.47 but **have not been run
yet** (jj was not installed on the machine that wrote them); expect to fix
details on the first run. GitButler, gitta, Fossil and Pijul have no
scripts yet.
