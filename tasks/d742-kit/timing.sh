#!/bin/bash
# timing.sh: alternate base-src and final, 3 runs each, fresh index per run; prints ms per run
W=/var/tmp/agent-tmp-d745; SP=/tmp/claude-1000/-home-ai-dev-code-graph-mcp/6a2b193e-2df9-4601-9491-fda2d5de574d/scratchpad
DIRS="$SP/d745/c6k/pyfn $SP/d745/c6k/pycb $SP/d745/c6k/pyx $SP/d745/c6k/jsfn $SP/d745/c6k/jsx $SP/d745/c6k/jsm $SP/d745/scan/py $W/corpora/hono $W/corpora/stdlib"
for d in $DIRS; do
  line="$(basename "$(dirname "$d")")/$(basename "$d")"
  for i in 1 2 3; do
    for arm in base-src w1; do
      rm -rf "${d:?}/.code-graph"
      s=$(date +%s%N); (cd "$d" && "$W/bin/$arm" incremental-index --no-embed --quiet >/dev/null 2>&1); e=$(date +%s%N)
      line="$line $arm=$(( (e-s)/1000000 ))"
    done
  done
  echo "$line (load $(cut -d' ' -f1 /proc/loadavg))"
done
