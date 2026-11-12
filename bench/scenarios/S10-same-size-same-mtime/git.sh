#!/bin/sh
# S10 with Git.
. "$(dirname "$0")/../lib.sh"
need git
g init -q a && cd a || result fail "init failed"
printf 'aaaa\n' >f && g add -A && g commit -qm one || result fail "commit failed"
touch -r f ../stamp
sleep 3
g status --porcelain >/dev/null
printf 'bbbb\n' >f
touch -r ../stamp f
[ -n "$(g status --porcelain)" ] || result fail "false clean: the rewrite was not detected"
result pass "same-size rewrite with the old mtime detected"
