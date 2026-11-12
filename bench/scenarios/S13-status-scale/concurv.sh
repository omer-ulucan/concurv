#!/bin/sh
# S13: `status` on 1,000 and 10,000 files after a save (median of 5 warm
# runs), and an edit is seen. Timings are for comparison on one machine.
. "$(dirname "$0")/../lib.sh"
need "$CONCURV"
detail=""
for n in 1000 10000; do
    mkdir "r$n" && cd "r$n" && cv init >/dev/null || result fail "init failed"
    make_tree "$n"
    cv save -m tree >/dev/null || result fail "save failed"
    cv status >/dev/null
    ms=$(time_ms cv status)
    echo "edited" >d0/f0.txt
    cv --json status | grep -q '"path":"d0/f0.txt"' || result fail "an edit was not seen ($n files)"
    detail="$detail$n files: ${ms} ms; "
    cd ..
done
result pass "status: ${detail%; } (median of 5 warm runs); edits seen"
