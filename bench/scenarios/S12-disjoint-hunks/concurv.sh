#!/bin/sh
# S12: two agents edit the top and the bottom of one file in private
# workspaces; then both edit the same line. Disjoint edits must merge;
# overlapping ones must not be resolved silently.
. "$(dirname "$0")/../lib.sh"
need "$CONCURV"
mkdir a && cd a && cv init >/dev/null || result fail "init failed"
two_functions lib.rs
cv save -m base >/dev/null && cv publish >/dev/null || result fail "base failed"
cv workspace add ../b >/dev/null || result fail "workspace add failed"
edit_line lib.rs "    1" "    10"
cv save -m top >/dev/null && cv publish >/dev/null || result fail "top failed"
cd ../b || exit 1
edit_line lib.rs "    2" "    20"
cv save -m bottom >/dev/null || result fail "bottom failed"
cv update >/dev/null 2>&1 || result fail "disjoint edits did not merge"
grep -q '^    10$' lib.rs && grep -q '^    20$' lib.rs || result fail "the merge lost an edit"
cv publish >/dev/null || result fail "publish of the merge failed"
cd ../a && cv update >/dev/null || result fail "update failed"
edit_line lib.rs "    10" "    11"
cv save -m a2 >/dev/null && cv publish >/dev/null || result fail "a2 failed"
cd ../b || exit 1
edit_line lib.rs "    10" "    12"
cv save -m b2 >/dev/null || result fail "b2 failed"
cv update >/dev/null 2>&1
code=$?
[ "$code" -eq 43 ] || result fail "overlapping edits: exit $code, expected a refusal (43)"
grep -q '^    12$' lib.rs || result fail "the refused merge changed the working copy"
result pass "disjoint hunks merged; overlapping edit refused (exit 43), working copy untouched"
