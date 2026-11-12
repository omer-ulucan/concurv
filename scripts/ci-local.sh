#!/usr/bin/env bash
# Local CI: the same gate as .github/workflows/ci.yml, run on this machine.
#
#   scripts/ci-local.sh [REV]        # default REV: HEAD
#
# Works on Linux (including WSL), macOS and Git Bash on Windows. The commit is
# cloned into a scratch directory on the machine's own filesystem, so the
# result reflects exactly what is committed (no uncommitted files, no build
# products) and, in WSL, Unix semantics (symlinks, exec bits, FIFOs) hold:
# they do not on /mnt/c.
#
# Steps: rustfmt check; clippy -D warnings; all tests (--no-fail-fast);
# release build; `cargo check` on the MSRV toolchain (when installed).
# Everything uses --locked. Logs and a summary go to
# $CONCURV_CI_DIR/runs/<timestamp>-<sha>/ (default: ~/.cache/concurv-ci).
# Exit status: 0 only if every step passed.
#
# Environment:
#   CONCURV_CI_DIR     scratch and log directory
#   CONCURV_CI_MSRV    MSRV toolchain (default: rust-version from Cargo.toml)
#   CONCURV_CI_SKIP    space-separated steps to skip (fmt clippy test release msrv)
set -u

repo=$(cd "$(dirname "$0")/.." && pwd)
rev=${1:-HEAD}
sha=$(git -C "$repo" rev-parse --verify "$rev^{commit}") || {
  echo "ci-local: unknown revision: $rev" >&2
  exit 2
}
short=${sha:0:9}

if [ -z "${CONCURV_CI_DIR:-}" ]; then
  if [ -n "${LOCALAPPDATA:-}" ]; then
    CONCURV_CI_DIR="$(cygpath -u "$LOCALAPPDATA" 2>/dev/null || echo "$LOCALAPPDATA")/concurv-ci"
  else
    CONCURV_CI_DIR="$HOME/.cache/concurv-ci"
  fi
fi
src="$CONCURV_CI_DIR/src"
run="$CONCURV_CI_DIR/runs/$(date -u +%Y%m%dT%H%M%SZ)-$short"
mkdir -p "$run"
summary="$run/summary.txt"

# Fresh checkout of exactly this commit; build caches persist across runs.
rm -rf "$src"
git clone --quiet --no-checkout "$repo" "$src" || exit 1
git -C "$src" -c advice.detachedHead=false checkout --quiet --detach "$sha" || exit 1
export CARGO_TARGET_DIR="$CONCURV_CI_DIR/target"
export CARGO_TERM_COLOR=never
export RUST_BACKTRACE=1
cd "$src" || exit 1

msrv=${CONCURV_CI_MSRV:-$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)}
skip=" ${CONCURV_CI_SKIP:-} "

{
  echo "concurv local CI"
  echo "commit:   $sha ($(git log -1 --format=%s "$sha"))"
  echo "date:     $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "host:     $(uname -srmo 2>/dev/null || uname -a)"
  if [ -r /etc/os-release ]; then
    echo "os:       $(. /etc/os-release && echo "$PRETTY_NAME")"
  fi
  if [ -n "${WSL_DISTRO_NAME:-}" ]; then echo "wsl:      $WSL_DISTRO_NAME"; fi
  echo "rustc:    $(rustc --version)"
  echo "cargo:    $(cargo --version)"
} | tee "$summary"

failed=0
step() {
  local name=$1
  shift
  if [ "${skip#* "$name" }" != "$skip" ]; then
    printf '%-9s skipped\n' "$name:" | tee -a "$summary"
    return
  fi
  local start end
  start=$(date +%s)
  if "$@" >"$run/$name.log" 2>&1; then
    end=$(date +%s)
    printf '%-9s pass (%ss)\n' "$name:" "$((end - start))" | tee -a "$summary"
  else
    end=$(date +%s)
    failed=1
    printf '%-9s FAIL (%ss), log: %s\n' "$name:" "$((end - start))" "$run/$name.log" | tee -a "$summary"
    tail -n 30 "$run/$name.log"
  fi
}

step fmt cargo fmt --all -- --check
step clippy cargo clippy --workspace --all-targets --locked -- -D warnings
step test cargo test --workspace --all-targets --locked --no-fail-fast
if [ -f "$run/test.log" ]; then
  awk '/^test result:/ {
         for (i = 1; i <= NF; i++) {
           if ($(i) ~ /^passed;?$/) p += $(i - 1)
           if ($(i) ~ /^failed;?$/) f += $(i - 1)
           if ($(i) ~ /^ignored;?$/) g += $(i - 1)
         }
       }
       END { printf "tests:    %d passed, %d failed, %d ignored\n", p, f, g }' \
    "$run/test.log" | tee -a "$summary"
  grep -E '^test .* FAILED$' "$run/test.log" | head -n 40 | tee -a "$summary"
fi
step release cargo build --release --locked
if rustup run "$msrv" rustc --version >/dev/null 2>&1; then
  step msrv env CARGO_TARGET_DIR="$CONCURV_CI_DIR/target-msrv" \
    rustup run "$msrv" cargo check --workspace --all-targets --locked
else
  printf '%-9s not run (toolchain %s not installed: rustup toolchain install %s)\n' \
    "msrv:" "$msrv" "$msrv" | tee -a "$summary"
fi

if [ "$failed" = 0 ]; then
  echo "result:   PASS" | tee -a "$summary"
else
  echo "result:   FAIL" | tee -a "$summary"
fi
echo "logs:     $run"
exit "$failed"
