#!/bin/sh
# S5 with Jujutsu: both workspaces move bookmark main from the same commit.
# NOT YET RUN (jj was not installed when this was written).
. "$(dirname "$0")/../lib.sh"
need "$JJ"
mkdir a && cd a && jjq git init >/dev/null 2>&1 || result fail "init failed"
echo base >f && jjq commit -m base >/dev/null 2>&1 || result fail "base failed"
jjq bookmark create main -r @- >/dev/null 2>&1 || result fail "bookmark failed"
jjq workspace add ../b >/dev/null 2>&1 || result fail "workspace add failed"
echo a >fa && jjq commit -m a >/dev/null 2>&1
(cd ../b && echo b >fb && jjq commit -m b >/dev/null 2>&1)
jjq bookmark set main -r @- >/dev/null 2>&1 || result fail "A's bookmark set failed"
cd ../b || exit 1
if jjq bookmark set main -r @- >/dev/null 2>&1; then
    result fail "the second bookmark move was accepted (last writer wins)"
fi
result pass "the second move of main (sideways) was refused"
