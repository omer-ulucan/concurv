#!/bin/sh
# S9: going back to an older snapshot over an unsaved edit. The edit must
# not be destroyed: refused, or recoverable from a backup.
. "$(dirname "$0")/../lib.sh"
need "$CONCURV"
mkdir a && cd a && cv init >/dev/null || result fail "init failed"
echo one >f
old=$(cv --json save -m one | sed -n 's/.*"snapshot":"\([0-9a-f]*\)".*/\1/p')
echo two >f && cv save -m two >/dev/null || result fail "save failed"
echo unsaved >f
cv restore --snapshot "$old" >/dev/null 2>&1
code=$?
[ "$code" -eq 20 ] || result fail "restore over an unsaved edit: exit $code, expected 20"
[ "$(cat f)" = unsaved ] || result fail "the refused restore changed the file"
backup=$(cv --json restore --snapshot "$old" --force | sed -n 's/.*"backup":"\([0-9a-f]*\)".*/\1/p')
[ "$(cat f)" = one ] || result fail "restore --force did not restore"
[ -n "$backup" ] || result fail "no backup snapshot recorded"
cv restore --snapshot "$backup" --force >/dev/null || result fail "restoring the backup failed"
[ "$(cat f)" = unsaved ] || result fail "the backup does not hold the unsaved edit"
result pass "refused (exit 20) without --force; with --force a backup snapshot kept the unsaved edit"
