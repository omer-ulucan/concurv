#!/usr/bin/env bash
# Run every scenario (bench/scenarios/S*/<tool>.sh) for every tool and
# write a results file with the environment record the comparison plan
# requires (docs/benchmarks/comparison-plan.md).
#
#   bench/scenarios/run.sh [OUT]     # default OUT: bench/results/<date>-<os>.md
#
# Environment: CONCURV (default: target/release/concurv[.exe]), JJ (default:
# jj), TOOLS (default: "concurv git jj"). A tool that is not installed is
# reported as `unsupported`, never skipped silently. No host name or user
# name is written.
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
if [ -z "${CONCURV:-}" ]; then
    CONCURV=$root/target/release/concurv
    [ -x "$CONCURV.exe" ] && CONCURV=$CONCURV.exe
fi
export CONCURV JJ=${JJ:-jj}
tools=${TOOLS:-"concurv git jj"}
os=$(uname -s | tr '[:upper:]' '[:lower:]' | cut -d_ -f1)
out=${1:-$root/bench/results/$(date -u +%Y-%m-%d)-$os.md}
mkdir -p "$(dirname "$out")"

version() {
    case $1 in
    concurv) "$CONCURV" --version 2>/dev/null || echo "not installed" ;;
    git) git --version 2>/dev/null || echo "not installed" ;;
    jj) "$JJ" --version 2>/dev/null || echo "not installed" ;;
    esac
}

pretty_os=$(uname -srm)
if [ -r /etc/os-release ]; then
    pretty_os="$(. /etc/os-release && echo "$PRETTY_NAME") ($pretty_os)"
fi
fs=$(df -PT "${TMPDIR:-/tmp}" 2>/dev/null | awk 'NR == 2 { print $2 }')
cpu=$(sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo 2>/dev/null | head -1)

{
    echo "# Scenario results, $(date -u +%Y-%m-%dT%H:%MZ)"
    echo
    echo "Produced by \`bench/scenarios/run.sh\`; each cell is the \`RESULT\` line of"
    echo "\`bench/scenarios/<scenario>/<tool>.sh\` (the exact commands)."
    echo
    echo "| Environment | |"
    echo "|---|---|"
    echo "| OS | $pretty_os |"
    echo "| Temp filesystem | ${fs:-unknown} |"
    echo "| CPU | ${cpu:-unknown}, $(nproc 2>/dev/null || echo '?') threads |"
    echo "| concurv commit | $(git -C "$root" rev-parse HEAD 2>/dev/null) |"
    for t in $tools; do echo "| $t | $(version "$t") |"; done
    echo
    printf '| Scenario |'
    for t in $tools; do printf ' %s |' "$t"; done
    echo
    printf '|---|'
    for t in $tools; do printf -- '---|'; done
    echo
    for dir in "$here"/S*/; do
        id=$(basename "$dir")
        printf '| %s |' "$id"
        for t in $tools; do
            script=$dir$t.sh
            if [ ! -f "$script" ]; then
                cell="(no script)"
            else
                line=$(sh "$script" 2>/dev/null | grep '^RESULT: ' | tail -1)
                cell=${line#RESULT: }
                cell="**${cell%% *}**: ${cell#* }"
                [ -n "$line" ] || cell="**error**: the script printed no RESULT"
            fi
            printf ' %s |' "$cell"
            echo "$id/$t: $cell" >&2
        done
        echo
    done
} >"$out"
echo "results: $out" >&2
