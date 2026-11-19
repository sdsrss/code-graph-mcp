#!/usr/bin/env bash
set -uo pipefail
W=/var/tmp/agent-tmp-d898; T=/home/ai/dev/code-graph-mcp/tasks/d898-references-fanout
cd "$W"
date
for rep in 1 2 3; do
  for b in head0 head3; do
    echo "== replay cgm $b rep $rep"; python3 "$T/replay.py" "bin/$b" /home/ai/dev/code-graph-mcp v0.180.0 v0.198.0 "$W/r-cgm-$b-$rep"
    rm -rf "${W:?}/r-cgm-$b-$rep"
    echo "== replay daagu $b rep $rep"; python3 "$T/replay.py" "bin/$b" /home/ai/dev/daagu 10b3ce5d~200 10b3ce5d "$W/r-daagu-$b-$rep"
    rm -rf "${W:?}/r-daagu-$b-$rep"
  done
done
date
echo "== fulldiff"; python3 "$T/fulldiff.py" bin/head0 bin/head3 "$W/corpora" "$W/full"
date
echo "== cost append"; python3 "$T/cost.py" "$W/cost-append" "$W/corpora/std" 7 append:shutil.py bin/head0 bin/head3
echo "== cost define int"; python3 "$T/cost.py" "$W/cost-int" "$W/corpora/std" 3 define:int bin/head0 bin/head3
echo "== cost define exc"; python3 "$T/cost.py" "$W/cost-exc" "$W/corpora/std" 3 define:exc bin/head0 bin/head3
date
