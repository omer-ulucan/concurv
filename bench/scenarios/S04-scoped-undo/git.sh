#!/bin/sh
# S4 with Git: no undo command. The closest is resetting A's branch through
# its worktree's HEAD reflog, which only touches A's branch.
. "$(dirname "$0")/../lib.sh"
need git
g init -q a && cd a || result fail "init failed"
echo base >f && g add -A && g commit -qm base || result fail "base failed"
g worktree add -q -b b ../b || result fail "worktree add failed"
echo a1 >fa && g add -A && g commit -qm a1
(cd ../b && echo b1 >fb && g add -A && g commit -qm b1)
echo a2 >fa && g add -A && g commit -qm a2
(cd ../b && echo b2 >fb && g add -A && g commit -qm b2)
g reset -q --hard 'HEAD@{1}' || result fail "reset through the reflog failed"
[ "$(g log -1 --format=%s)" = a1 ] || result fail "A's head is not a1"
cd ../b || exit 1
[ "$(g log -1 --format=%s)" = b2 ] || result fail "B's head changed"
result partial "no undo command; 'reset --hard HEAD@{1}' in A's worktree reverted only A (and discards A's uncommitted files)"
