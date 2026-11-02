#!/usr/bin/env bash
export HOME=/var/tmp/d370-review/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1
TIMEFORMAT='user=%U sys=%S wall=%R'
for i in 1 2 3; do
  for b in base head; do
    rm -rf "/var/tmp/d370-review/perf/std-${b:?}/.code-graph"
    cd "/var/tmp/d370-review/perf/std-$b" || exit 1
    t=$( { time /var/tmp/d370-review/parity/bin/$b incremental-index --no-embed >/dev/null 2>&1 ; } 2>&1 )
    echo "$b run$i $t"
  done
done
