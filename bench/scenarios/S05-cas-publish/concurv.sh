#!/bin/sh
# S5: A and B both start from the same head of main and publish. Exactly one
# wins; the other is refused and nothing is lost.
. "$(dirname "$0")/../lib.sh"
need "$CONCURV"
mkdir a && cd a && cv init >/dev/null || result fail "init failed"
echo base >f && cv save -m base >/dev/null && cv publish >/dev/null || result fail "base failed"
cv workspace add ../b >/dev/null || result fail "workspace add failed"
echo a >fa && cv save -m a >/dev/null || result fail "save a failed"
(cd ../b && echo b >fb && cv save -m b >/dev/null) || result fail "save b failed"
cv publish >/dev/null || result fail "A's publish failed"
cd ../b || exit 1
cv publish >/dev/null 2>&1
code=$?
[ "$code" -eq 42 ] || result fail "B's publish: exit $code, expected a refusal (42)"
msg=$(cd ../a && cv --json log -n 1 | sed -n 's/.*"message":"\([^"]*\)".*/\1/p')
[ "$msg" = a ] || result fail "main is not A's snapshot"
[ "$(cv --json log -n 1 | sed -n 's/.*"message":"\([^"]*\)".*/\1/p')" = b ] || result fail "B's save was lost"
result pass "first publish won; the second was refused (exit 42) and B's save stays in B"
