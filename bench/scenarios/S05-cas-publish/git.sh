#!/bin/sh
# S5 with Git: both worktrees move main with a compare-and-swap
# (`update-ref <ref> <new> <old>`), expecting the same old head.
. "$(dirname "$0")/../lib.sh"
need git
g init -q a && cd a || result fail "init failed"
echo base >f && g add -A && g commit -qm base || result fail "base failed"
base=$(g rev-parse HEAD)
g worktree add -q --detach ../b || result fail "worktree add failed"
g switch -q -c wa && echo a >fa && g add -A && g commit -qm a
(cd ../b && g switch -q -c wb && echo b >fb && g add -A && g commit -qm b)
ca=$(g rev-parse wa)
cb=$(g rev-parse wb)
g update-ref refs/heads/main "$ca" "$base" || result fail "A's update-ref failed"
if (cd ../b && g update-ref refs/heads/main "$cb" "$base" 2>/dev/null); then
    result fail "both compare-and-swaps succeeded"
fi
[ "$(g rev-parse main)" = "$ca" ] || result fail "main is not A's commit"
result pass "first update-ref won; the second (same old value) was refused; B's commit stays on wb"
