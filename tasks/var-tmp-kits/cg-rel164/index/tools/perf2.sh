#!/usr/bin/env bash
# perf.sh RUNS corpus...  -> appends "corpus arm iter elapsed_ms wall user sys" to perf.tsv
set -uo pipefail
RUNS=$1; shift
OUT=${OUT:-/var/tmp/cg-rel164/index/perf2.tsv}
for n in "$@"; do
  for arm in basesrc cand; do rm -rf "/var/tmp/cg-rel164/index/perf/$arm/$n"; mkdir -p /var/tmp/cg-rel164/index/perf/$arm; cp -a /var/tmp/cg-rel164/index/corp/pristine/$n /var/tmp/cg-rel164/index/perf/$arm/$n; done
  for i in $(seq 1 $RUNS); do
    if [ $((i % 2)) -eq 1 ]; then order="basesrc cand"; else order="cand basesrc"; fi
    for arm in $order; do
      D=/var/tmp/cg-rel164/index/perf/$arm/$n; rm -rf "${D:?}/.code-graph"
      cd "$D"
      { TIMEFORMAT="%R %U %S"; time env HOME=/var/tmp/cg-rel164/index/home CODE_GRAPH_NO_GITIGNORE=1 /var/tmp/cg-rel164/index/bin/$arm incremental-index --no-embed --json > /var/tmp/cg-rel164/index/perf/j.$$ 2>/dev/null; } 2> /var/tmp/cg-rel164/index/perf/t.$$
      ms=$(python3 -c 'import json;print(json.load(open("/var/tmp/cg-rel164/index/perf/j.'$$'"))["elapsed_ms"])')
      echo -e "$n\t$arm\t$i\t$ms\t$(cat /var/tmp/cg-rel164/index/perf/t.$$ | tr ' ' '\t')\t$(cut -d' ' -f1 /proc/loadavg)" >> $OUT
    done
  done
done
rm -f /var/tmp/cg-rel164/index/perf/t.$$ /var/tmp/cg-rel164/index/perf/j.$$
