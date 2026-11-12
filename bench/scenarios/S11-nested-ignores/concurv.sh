#!/bin/sh
# S11: nested ignore files with negation and an excluded parent; a file
# ignored after it was recorded stays tracked.
. "$(dirname "$0")/../lib.sh"
need "$CONCURV"
mkdir a && cd a && cv init >/dev/null || result fail "init failed"
ignore_fixture
cv save -m first >/dev/null && cv publish >/dev/null || result fail "save failed"
# What was recorded: a fresh workspace checks out exactly the snapshot.
cv workspace add ../w >/dev/null || result fail "workspace add failed"
files=$(cd ../w && find . -type f ! -path './.concurv/*' | sed 's|^\./||' | sort | tr '\n' ' ' | sed 's/ $//')
[ "$files" = "$IGNORE_EXPECTED" ] || result fail "recorded: '$files', expected: '$IGNORE_EXPECTED'"
printf 'later.txt\n' >>.gitignore
echo changed >later.txt
cv --json status | grep -q '"path":"later.txt"' || result fail "a tracked file stopped being tracked once ignored"
result pass "recorded the expected set; tracked-then-ignored stays tracked"
