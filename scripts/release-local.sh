#!/usr/bin/env bash
# Build release archives for a tag on this machine, from Git Bash on
# Windows: Windows x86_64 natively and Linux x86_64 (static, musl) in WSL.
# Each build starts from a fresh clone of the tag. Writes the archives and
# SHA256SUMS to OUTDIR; publishes nothing (see .github/workflows/release.yml
# for the archive layout, which this script reproduces).
#
#   scripts/release-local.sh <tag> [OUTDIR]   # default OUTDIR: target/dist/<tag>
#
# Needs: Rust with the x86_64-pc-windows-msvc target (Windows) and, in the
# WSL distribution (CONCURV_WSL_DISTRO, default Ubuntu), Rust with the
# x86_64-unknown-linux-musl target and gcc.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
tag=${1:?usage: scripts/release-local.sh <tag> [OUTDIR]}
sha=$(git -C "$repo" rev-parse --verify "$tag^{commit}")
out=${2:-$repo/target/dist/$tag}
distro=${CONCURV_WSL_DISTRO:-Ubuntu}
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Small binaries: no debug info, symbols stripped.
export CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=symbols

echo "== Windows x86_64 ($tag = $sha)"
git clone -q "$repo" "$work/src"
git -C "$work/src" -c advice.detachedHead=false checkout -q --detach "$sha"
(cd "$work/src" && cargo build --release --locked -p concurv-cli --target x86_64-pc-windows-msvc)
name="concurv-$tag-x86_64-pc-windows-msvc"
mkdir "$work/$name"
cp "$work/src/target/x86_64-pc-windows-msvc/release/concurv.exe" \
  "$work/src/target/x86_64-pc-windows-msvc/release/concurvd.exe" \
  "$work/src/README.md" "$work/src/CHANGELOG.md" "$work/$name/"
(cd "$work" && /c/Windows/System32/tar.exe -a -c -f "$name.zip" "$name")
mv "$work/$name.zip" "$out/"
"$work/$name/concurv.exe" --version

echo "== Linux x86_64, static musl, in WSL ($distro)"
wslrepo=$(wsl.exe -d "$distro" --exec wslpath -a "$(cygpath -m "$repo")" | tr -d '\r')
wslout=$(wsl.exe -d "$distro" --exec wslpath -a "$(cygpath -m "$out")" | tr -d '\r')
# --exec: no extra parse by the login shell; values travel as arguments.
wsl.exe -d "$distro" --exec bash -lc '
  set -euo pipefail
  repo=$1 sha=$2 tag=$3 out=$4
  w=$(mktemp -d); trap "rm -rf \"$w\"" EXIT
  git clone -q "$repo" "$w/src"
  cd "$w/src" && git -c advice.detachedHead=false checkout -q --detach "$sha"
  export CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=symbols
  export CC_x86_64_unknown_linux_musl=gcc CARGO_TARGET_DIR="$HOME/.cache/concurv-release/target"
  cargo build --release --locked -p concurv-cli --target x86_64-unknown-linux-musl
  n="concurv-$tag-x86_64-unknown-linux-musl"
  bin="$CARGO_TARGET_DIR/x86_64-unknown-linux-musl/release"
  mkdir "$w/$n"
  cp "$bin/concurv" "$bin/concurvd" README.md CHANGELOG.md "$w/$n/"
  file "$w/$n/concurv" || true
  "$w/$n/concurv" --version
  tar -C "$w" -czf "$out/$n.tar.gz" "$n"
' bash "$wslrepo" "$sha" "$tag" "$wslout"

cd "$out"
sha256sum ./*.zip ./*.tar.gz | sed 's| \./| |' >SHA256SUMS
cat SHA256SUMS
echo "release-local: archives in $out"
