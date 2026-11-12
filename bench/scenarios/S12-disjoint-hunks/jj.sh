#!/bin/sh
# S12 with Jujutsu: one workspace per agent; a merge commit with both
# parents. NOT YET RUN (jj was not installed when this was written).
. "$(dirname "$0")/../lib.sh"
need "$JJ"
mkdir a && cd a && jjq git init >/dev/null 2>&1 || result fail "init failed"
two_functions lib.rs
jjq commit -m base >/dev/null 2>&1 || result fail "base failed"
jjq workspace add ../b >/dev/null 2>&1 || result fail "workspace add failed"
edit_line lib.rs "    1" "    10"
jjq commit -m top >/dev/null 2>&1 || result fail "top failed"
cd ../b || exit 1
edit_line lib.rs "    2" "    20"
jjq commit -m bottom >/dev/null 2>&1 || result fail "bottom failed"
jjq new 'description(substring:"top")' 'description(substring:"bottom")' >/dev/null 2>&1 \
    || result fail "merge commit failed"
grep -q '^    10$' lib.rs && grep -q '^    20$' lib.rs || result fail "the merge lost an edit"
jjq commit -m merged >/dev/null 2>&1 || result fail "commit of the merge failed"
cd ../a || exit 1
jjq new 'description(substring:"merged")' >/dev/null 2>&1 || result fail "new failed"
edit_line lib.rs "    10" "    11"
jjq commit -m a2 >/dev/null 2>&1 || result fail "a2 failed"
cd ../b || exit 1
edit_line lib.rs "    10" "    12"
jjq commit -m b2 >/dev/null 2>&1 || result fail "b2 failed"
jjq new 'description(substring:"a2")' 'description(substring:"b2")' >/dev/null 2>&1 \
    || result fail "merge commit failed"
conflict=$(jjq log --no-graph -r @ -T 'if(conflict, "conflict", "clean")' 2>/dev/null)
[ "$conflict" = conflict ] || result fail "overlapping edits merged without a conflict"
result pass "disjoint hunks merged; overlapping edit recorded as a conflicted commit"
