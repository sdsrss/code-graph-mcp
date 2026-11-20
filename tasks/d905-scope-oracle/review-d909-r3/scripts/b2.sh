#!/bin/bash
W=/var/tmp/agent-tmp-d909-review3
T=/home/ai/dev/code-graph-mcp/tasks
cd $W/work || exit 1
until [ -f $W/logs/se-done.txt ]; do sleep 5; done
python3 $W/scripts/pairdiff.py c52b115c f06f5e96 $W/scripts/gen_r3.py 130000 600 5 $W/work/pd1 4 > $W/logs/pd-c52-f06-r3.txt 2>&1
python3 $W/scripts/pairdiff.py c52b115c f06f5e96 $T/d905-scope-oracle/review-d909-r2/scripts/gen_scope.py 230000 600 5 $W/work/pd2 4 > $W/logs/pd-c52-f06-scope.txt 2>&1
python3 $W/scripts/dynjudge.py $W/scripts/gen_dyn.py 140000 500 4 $W/work/dj 4 v0.203.0 c52b115c f06f5e96 > $W/logs/dyn.txt 2>&1
python3 $T/d905-scope-oracle/removed_judge.py $W/bin/v0.203.0 $W/bin/f06f5e96 $W/scripts/gen_r3.py 150000 500 5 $W/work/rj1 4 > $W/logs/rj-v203-f06-r3.txt 2>&1
python3 $T/d905-scope-oracle/removed_judge.py $W/bin/c52b115c $W/bin/f06f5e96 $W/scripts/gen_r3.py 150000 500 5 $W/work/rj2 4 > $W/logs/rj-c52-f06-r3.txt 2>&1
python3 $T/d905-scope-oracle/removed_judge.py $W/bin/v0.203.0 $W/bin/f06f5e96 $T/d905-scope-oracle/review-d909-r2/scripts/gen_scope.py 250000 500 5 $W/work/rj3 4 > $W/logs/rj-v203-f06-scope.txt 2>&1
rm -rf "${W:?}/work/pd1" "${W:?}/work/pd2" "${W:?}/work/dj" "${W:?}/work/rj1" "${W:?}/work/rj2" "${W:?}/work/rj3"
echo B2DONE > $W/logs/b2-done.txt
$W/fz.sh
