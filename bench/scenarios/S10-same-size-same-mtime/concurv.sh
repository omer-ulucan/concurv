#!/bin/sh
# S10: a file rewritten with the same size and its old modification time
# put back (cf. jj #10509). The change must be seen.
. "$(dirname "$0")/../lib.sh"
need "$CONCURV"
mkdir a && cd a && cv init >/dev/null || result fail "init failed"
printf 'aaaa\n' >f
cv save -m one >/dev/null || result fail "save failed"
touch -r f ../stamp
sleep 3
cv status >/dev/null
printf 'bbbb\n' >f
touch -r ../stamp f
cv --json status | grep -q '"clean":false' || result fail "false clean: the rewrite was not detected"
cv --json status --no-cache | grep -q '"clean":false' || result fail "false clean without the cache"
result pass "same-size rewrite with the old mtime detected"
