#!/bin/bash
W=/var/tmp/agent-tmp-d909-review3
T=/home/ai/dev/code-graph-mcp/tasks
SE=$T/d905-scope-oracle/subset_enum.py
cd $W/work || exit 1
run() { # name base head gen first count steps
  python3 $SE $W/bin/$2 $W/bin/$3 $4 $5 $6 $7 $W/work/se-$1 4 > $W/logs/se-$1.txt 2>&1
  echo "rc=$?" >> $W/logs/se-$1.txt
  rm -rf "${W:?}/work/se-$1"
}
run scope-v203 v0.203.0 f06f5e96 $T/d905-scope-oracle/review-d909-r2/scripts/gen_scope.py 200000 1000 5
run fuzzdef-v203 v0.203.0 f06f5e96 $T/d893-fuzz/fuzz_seq.py 300000 600 5
run review-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gen_review.py 400000 600 5
run pymove-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gens/gen_pymove.py 500000 600 5
run js-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gens/gen_js.py 600000 500 5
run ts-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gens/gen_ts.py 650000 400 5
run rs-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gens/gen_rs.py 700000 400 5
run dart-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gens/gen_dart.py 800000 300 5
run go-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gens/gen_go2.py 900000 300 5
run multi-v203 v0.203.0 f06f5e96 $T/d893-fuzz/gens/gen_multi.py 950000 300 5
run js-c52 c52b115c f06f5e96 $T/d893-fuzz/gens/gen_js.py 600000 500 5
run rs-c52 c52b115c f06f5e96 $T/d893-fuzz/gens/gen_rs.py 700000 400 5
run r3b-v203 v0.203.0 f06f5e96 $W/scripts/gen_r3.py 110000 1500 5
echo ALLDONE > $W/logs/se-done.txt
