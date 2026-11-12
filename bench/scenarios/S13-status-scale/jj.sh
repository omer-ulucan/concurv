#!/bin/sh
# S13 with Jujutsu: `jj status` (snapshots the working copy). NOT YET RUN.
. "$(dirname "$0")/../lib.sh"
need "$JJ"
detail=""
for n in 1000 10000; do
    mkdir "r$n" && cd "r$n" && jjq git init >/dev/null 2>&1 || result fail "init failed"
    make_tree "$n"
    jjq commit -m tree >/dev/null 2>&1 || result fail "commit failed"
    jjq status >/dev/null 2>&1
    ms=$(time_ms jjq status)
    echo "edited" >d0/f0.txt
    jjq diff --name-only 2>/dev/null | grep -q 'd0/f0.txt' || result fail "an edit was not seen ($n files)"
    detail="$detail$n files: ${ms} ms; "
    cd ..
done
result pass "status: ${detail%; } (median of 5 warm runs); edits seen"
