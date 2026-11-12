#!/bin/sh
# S9 with Git: `git restore --source` to an older commit over an
# uncommitted edit.
. "$(dirname "$0")/../lib.sh"
need git
g init -q a && cd a || result fail "init failed"
echo one >f && g add -A && g commit -qm one || result fail "commit failed"
echo two >f && g commit -qam two || result fail "commit failed"
echo unsaved >f
if ! g restore --source=HEAD~1 -- f 2>/dev/null; then
    [ "$(cat f)" = unsaved ] && result pass "restore refused; the edit is intact"
    result fail "restore failed and changed the file"
fi
[ "$(cat f)" = one ] || result fail "restore did not restore"
# Was the unsaved content kept anywhere (an object Git wrote)?
blob=$(printf 'unsaved\n' | g hash-object --stdin)
if g cat-file -e "$blob" 2>/dev/null; then
    result partial "overwritten, but the content survives as an unreferenced object"
fi
result fail "the uncommitted edit was overwritten without warning or backup"
