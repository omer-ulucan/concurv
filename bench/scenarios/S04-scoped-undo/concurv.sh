#!/bin/sh
# S4: agents A and B save alternately in their own workspaces; A undoes.
# Only A's most recent operation may be reverted; B is untouched.
. "$(dirname "$0")/../lib.sh"
need "$CONCURV"
head_msg() { cv --json log -n 1 | sed -n 's/.*"message":"\([^"]*\)".*/\1/p'; }
mkdir a && cd a && cv init >/dev/null || result fail "init failed"
echo base >f && cv save -m base >/dev/null && cv publish >/dev/null || result fail "base failed"
cv workspace add ../b >/dev/null || result fail "workspace add failed"
echo a1 >fa && cv save -m a1 >/dev/null
(cd ../b && echo b1 >fb && cv save -m b1 >/dev/null)
echo a2 >fa && cv save -m a2 >/dev/null
(cd ../b && echo b2 >fb && cv save -m b2 >/dev/null)
cv undo >/dev/null || result fail "undo failed"
[ "$(head_msg)" = a1 ] || result fail "A's head is '$(head_msg)', expected a1"
cd ../b || exit 1
[ "$(head_msg)" = b2 ] || result fail "B's head changed to '$(head_msg)'"
[ "$(cat fb)" = b2 ] || result fail "B's files changed"
result pass "undo in A reverted A's last save only; B's head and files unchanged"
