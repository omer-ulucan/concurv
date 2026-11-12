#!/bin/sh
# S9 with Jujutsu: `jj restore --from` an older commit over an edit. jj
# snapshots the working copy before every command, so the edit should be
# recoverable from the operation log. NOT YET RUN (jj was not installed
# when this was written).
. "$(dirname "$0")/../lib.sh"
need "$JJ"
mkdir a && cd a && jjq git init >/dev/null 2>&1 || result fail "init failed"
echo one >f && jjq commit -m one >/dev/null 2>&1 || result fail "commit failed"
echo two >f && jjq commit -m two >/dev/null 2>&1 || result fail "commit failed"
echo unsaved >f
jjq restore --from 'description(substring:"one")' >/dev/null 2>&1 || result fail "restore failed"
[ "$(cat f)" = one ] || result fail "restore did not restore"
jjq undo >/dev/null 2>&1 || result fail "undo failed"
[ "$(cat f)" = unsaved ] || result fail "the edit could not be recovered with jj undo"
result pass "restored at once (no refusal); the edit was recoverable with jj undo"
