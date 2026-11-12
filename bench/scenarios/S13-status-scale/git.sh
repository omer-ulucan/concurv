#!/bin/sh
# S13 with Git: `git status` after a commit.
. "$(dirname "$0")/../lib.sh"
need git
detail=""
for n in 1000 10000; do
    g init -q "r$n" && cd "r$n" || result fail "init failed"
    make_tree "$n"
    g add -A && g commit -qm tree || result fail "commit failed"
    g status --porcelain >/dev/null
    ms=$(time_ms g status --porcelain)
    echo "edited" >d0/f0.txt
    g status --porcelain | grep -q 'd0/f0.txt' || result fail "an edit was not seen ($n files)"
    detail="$detail$n files: ${ms} ms; "
    cd ..
done
result pass "status: ${detail%; } (median of 5 warm runs); edits seen"
