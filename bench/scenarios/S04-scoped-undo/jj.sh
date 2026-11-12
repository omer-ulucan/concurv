#!/bin/sh
# S4 with Jujutsu: workspaces A and B commit alternately; A runs `jj undo`.
# The operation log is shared by all workspaces: which operation does A's
# undo revert? NOT YET RUN (jj was not installed when this was written).
. "$(dirname "$0")/../lib.sh"
need "$JJ"
has() { [ -n "$(jjq log --no-graph -r "description(substring:\"$1\")" -T 'commit_id' 2>/dev/null)" ]; }
mkdir a && cd a && jjq git init >/dev/null 2>&1 || result fail "init failed"
echo base >f && jjq commit -m base >/dev/null 2>&1 || result fail "base failed"
jjq workspace add ../b >/dev/null 2>&1 || result fail "workspace add failed"
echo a1 >fa && jjq commit -m a1 >/dev/null 2>&1
(cd ../b && echo b1 >fb && jjq commit -m b1 >/dev/null 2>&1)
echo a2 >fa && jjq commit -m a2 >/dev/null 2>&1
(cd ../b && echo b2 >fb && jjq commit -m b2 >/dev/null 2>&1)
jjq undo >/dev/null 2>&1 || result fail "undo failed"
if ! has b2; then
    result fail "undo in A reverted B's last commit (b2 is gone)"
fi
if has a2; then
    result partial "undo in A kept B's work but did not revert A's last commit"
fi
result pass "undo in A reverted A's last commit only; B's commits remain"
