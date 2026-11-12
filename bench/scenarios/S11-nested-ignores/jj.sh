#!/bin/sh
# S11 with Jujutsu (reads .gitignore files). NOT YET RUN.
. "$(dirname "$0")/../lib.sh"
need "$JJ"
mkdir a && cd a && jjq git init >/dev/null 2>&1 || result fail "init failed"
ignore_fixture
jjq commit -m first >/dev/null 2>&1 || result fail "commit failed"
files=$(jjq file list -r @- 2>/dev/null | sort | tr '\n' ' ' | sed 's/ $//')
[ "$files" = "$IGNORE_EXPECTED" ] || result fail "recorded: '$files', expected: '$IGNORE_EXPECTED'"
printf 'later.txt\n' >>.gitignore
echo changed >later.txt
jjq diff --name-only 2>/dev/null | grep -q '^later.txt$' || result fail "a tracked file stopped being tracked once ignored"
result pass "recorded the expected set; tracked-then-ignored stays tracked"
