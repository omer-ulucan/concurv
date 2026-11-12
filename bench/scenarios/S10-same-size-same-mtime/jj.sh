#!/bin/sh
# S10 with Jujutsu (cf. jj #10509). NOT YET RUN (jj was not installed when
# this was written).
. "$(dirname "$0")/../lib.sh"
need "$JJ"
mkdir a && cd a && jjq git init >/dev/null 2>&1 || result fail "init failed"
printf 'aaaa\n' >f && jjq commit -m one >/dev/null 2>&1 || result fail "commit failed"
touch -r f ../stamp
sleep 3
jjq status >/dev/null 2>&1
printf 'bbbb\n' >f
touch -r ../stamp f
[ -n "$(jjq diff --name-only 2>/dev/null)" ] || result fail "false clean: the rewrite was not detected"
result pass "same-size rewrite with the old mtime detected"
