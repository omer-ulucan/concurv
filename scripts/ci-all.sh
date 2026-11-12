#!/usr/bin/env bash
# Run the local CI gate (scripts/ci-local.sh) for one commit on Windows and in
# WSL at the same time, from Git Bash on Windows. Prints both summaries and
# exits non-zero if either platform fails.
#
#   scripts/ci-all.sh [REV]          # default REV: HEAD
#
# Environment: CONCURV_WSL_DISTRO (default: Ubuntu). macOS is not covered by
# this script; run scripts/ci-local.sh on a Mac for that.
set -u

repo=$(cd "$(dirname "$0")/.." && pwd)
sha=$(git -C "$repo" rev-parse --verify "${1:-HEAD}^{commit}") || exit 2
distro=${CONCURV_WSL_DISTRO:-Ubuntu}
out=$(mktemp -d)

winrepo=$(cygpath -m "$repo")
wslrepo=$(wsl.exe -d "$distro" -- wslpath -a "$winrepo" | tr -d '\r')
if [ -z "$wslrepo" ]; then
  echo "ci-all: WSL distribution $distro is not available" >&2
  exit 2
fi

echo "ci-all: $sha on Windows and WSL ($distro); output in $out"
bash "$repo/scripts/ci-local.sh" "$sha" >"$out/windows.txt" 2>&1 &
win=$!
wsl.exe -d "$distro" -- bash -lc "cd '$wslrepo' && bash scripts/ci-local.sh $sha" >"$out/wsl.txt" 2>&1 &
lin=$!
wait "$win"
wst=$?
wait "$lin"
lst=$?

for p in windows wsl; do
  echo
  echo "=== $p"
  tr -d '\r' <"$out/$p.txt" |
    grep -E '^(concurv local CI|(commit|date|host|os|wsl|rustc|cargo|fmt|clippy|test|tests|release|msrv|result|logs): )|^test .* FAILED$'
done
echo
if [ "$wst" = 0 ] && [ "$lst" = 0 ]; then
  echo "ci-all: PASS (windows, wsl)"
  exit 0
fi
echo "ci-all: FAIL (windows exit $wst, wsl exit $lst)"
exit 1
