#!/bin/sh
# S11 with Git.
. "$(dirname "$0")/../lib.sh"
need git
g init -q a && cd a || result fail "init failed"
ignore_fixture
g add -A && g commit -qm first || result fail "commit failed"
files=$(g ls-files | sort | tr '\n' ' ' | sed 's/ $//')
[ "$files" = "$IGNORE_EXPECTED" ] || result fail "recorded: '$files', expected: '$IGNORE_EXPECTED'"
printf 'later.txt\n' >>.gitignore
echo changed >later.txt
g status --porcelain | grep -q ' later.txt$' || result fail "a tracked file stopped being tracked once ignored"
result pass "recorded the expected set; tracked-then-ignored stays tracked"
