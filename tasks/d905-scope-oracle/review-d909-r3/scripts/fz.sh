#!/bin/bash
W=/var/tmp/agent-tmp-d909-review3
T=/home/ai/dev/code-graph-mcp/tasks
FZ=$T/d905-scope-oracle/review-d909-r2/scripts/fuzz_seqL.py
export FUZZ_WORK=$W/work/fuzzwork
mkdir -p $FUZZ_WORK $W/fuzz
cd $W/work || exit 1
run() { # name gen first count steps
  for a in v0.203.0 f06f5e96; do
    if [ -n "$2" ]; then g="--gen $2"; else g=""; fi
    python3 -I $FZ $W/bin/$a $3 $4 $W/fuzz/$1-$a.jsonl --steps $5 --jobs 4 $g > $W/fuzz/$1-$a.log 2>&1
  done
  python3 -I $T/d893-fuzz/fuzzcmp.py $W/fuzz/$1-v0.203.0.jsonl $W/fuzz/$1-f06f5e96.jsonl > $W/fuzz/$1-cmp.txt 2>&1
}
run r3 $W/scripts/gen_r3.py 120000 1000 6
run scope $T/d905-scope-oracle/review-d909-r2/scripts/gen_scope.py 220000 1000 6
run def "" 320000 800 4
run review $T/d893-fuzz/gen_review.py 420000 600 5
run pymove $T/d893-fuzz/gens/gen_pymove.py 520000 600 4
echo FZDONE > $W/fuzz/done.txt
