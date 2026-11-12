# Shared helpers for bench/scenarios/<id>/<tool>.sh (POSIX sh).
#
# Every scenario script runs in a fresh temporary directory, prints exactly
# one line `RESULT: <pass|partial|fail|unsupported> <detail>` and exits 0
# (a crash of the script itself is reported by the driver as `error`).
# Tools come from PATH; concurv from $CONCURV (default: `concurv`).

set -u
CONCURV=${CONCURV:-concurv}
JJ=${JJ:-jj}

# Hermetic Git and jj: no user configuration, fixed identities.
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_AUTHOR_NAME=Bench GIT_AUTHOR_EMAIL=bench@example.com
export GIT_COMMITTER_NAME=Bench GIT_COMMITTER_EMAIL=bench@example.com
export CONCURV_AUTHOR="Bench <bench@example.com>"
export CONCURV_CONFIG=""
export JJ_CONFIG=/dev/null
export JJ_USER=Bench JJ_EMAIL=bench@example.com

result() {
    printf 'RESULT: %s %s\n' "$1" "$2"
    exit 0
}

# Skip unless the tool is installed.
need() {
    command -v "$1" >/dev/null 2>&1 || result unsupported "$1 is not installed"
}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK" || exit 1

g() { git -c core.autocrlf=false -c init.defaultBranch=main "$@"; }
cv() { "$CONCURV" "$@"; }
jjq() { "$JJ" --no-pager --color=never "$@"; }

# A file with two functions far apart (ten lines between them).
two_functions() {
    {
        echo "fn top() {"
        echo "    1"
        echo "}"
        i=0
        while [ $i -lt 10 ]; do echo "// filler $i"; i=$((i + 1)); done
        echo "fn bottom() {"
        echo "    2"
        echo "}"
    } >"$1"
}

# Replace a whole line matching $2 with $3 in file $1 (portable sed -i).
edit_line() {
    sed "s|^$2\$|$3|" "$1" >"$1.tmp" && mv "$1.tmp" "$1"
}

# The S11 fixture: nested ignore files with negation, an excluded parent,
# and a file that becomes ignored after it was recorded.
ignore_fixture() {
    printf '*.log\nbuild/\n!build/keep.txt\n!keep.log\n' >.gitignore
    mkdir -p sub build deep/er
    printf '!*.log\n' >sub/.gitignore
    echo x >a.log
    echo x >keep.log
    echo x >sub/b.log
    echo x >build/out.o
    echo x >build/keep.txt
    echo x >deep/er/c.log
    echo x >src.txt
    echo x >later.txt
}
# What every tool must record after the fixture and a first save/commit:
IGNORE_EXPECTED='.gitignore keep.log later.txt src.txt sub/.gitignore sub/b.log'

# Milliseconds since the epoch (GNU date).
now_ms() { echo $(($(date +%s%N) / 1000000)); }

# Median of the numbers on stdin.
median() { sort -n | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }

# A tree of $1 small files in directories of 100.
make_tree() {
    i=0
    while [ "$i" -lt "$1" ]; do
        d=d$((i / 100))
        [ -d "$d" ] || mkdir "$d"
        echo "file $i" >"$d/f$i.txt"
        i=$((i + 1))
    done
}

# Median wall time in ms of 5 runs of the command "$@" (output discarded).
time_ms() {
    n=0
    while [ "$n" -lt 5 ]; do
        t0=$(now_ms)
        "$@" >/dev/null 2>&1
        echo $(($(now_ms) - t0))
        n=$((n + 1))
    done | median
}
