#!/bin/sh
# S12 with Git: one worktree per agent, merges between branches.
. "$(dirname "$0")/../lib.sh"
need git
g init -q a && cd a || result fail "init failed"
two_functions lib.rs
g add -A && g commit -qm base || result fail "base failed"
g worktree add -q -b b ../b || result fail "worktree add failed"
edit_line lib.rs "    1" "    10"
g commit -qam top || result fail "top failed"
cd ../b || exit 1
edit_line lib.rs "    2" "    20"
g commit -qam bottom || result fail "bottom failed"
g merge -q --no-edit main >/dev/null 2>&1 || result fail "disjoint edits did not merge"
grep -q '^    10$' lib.rs && grep -q '^    20$' lib.rs || result fail "the merge lost an edit"
cd ../a && g merge -q --ff-only b >/dev/null 2>&1 || result fail "fast-forward of main failed"
edit_line lib.rs "    10" "    11"
g commit -qam a2 || result fail "a2 failed"
cd ../b || exit 1
edit_line lib.rs "    10" "    12"
g commit -qam b2 || result fail "b2 failed"
if g merge -q --no-edit main >/dev/null 2>&1; then
    result fail "overlapping edits merged without a conflict"
fi
grep -q '^<<<<<<<' lib.rs || result fail "conflict without markers"
result pass "disjoint hunks merged; overlapping edit reported as a conflict (markers written into the working copy)"
