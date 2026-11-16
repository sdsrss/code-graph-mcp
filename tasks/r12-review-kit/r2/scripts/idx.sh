#!/usr/bin/env bash
# idx.sh <corpus> <arm>  -> copies corpus, full index, snapshot to snap/<corpus>.<arm>.db
set -euo pipefail
W=/var/tmp/agent-tmp-r12-review2
c=$1; arm=$2
case $arm in v0193) B=/var/tmp/agent-tmp-converge-r12/bin/v0193;; rel) B=/var/tmp/agent-tmp-converge-r12/bin/rel;; r1) B=/var/tmp/agent-tmp-converge-r12/bin/r1;; *) B=$arm; arm=$(basename "$arm");; esac
D="${W:?}/corp/$c.$arm"
rm -rf "${D:?}"
cp -a /var/tmp/agent-tmp-converge-r12/corpora/$c "$D"
cd "$D"
s=$(date +%s.%N)
CODE_GRAPH_NO_AUTO_UPDATE=1 "$B" incremental-index --no-embed > "$W/snap/$c.$arm.log" 2>&1
e=$(date +%s.%N)
sqlite3 .code-graph/index.db ".backup $W/snap/$c.$arm.db"
echo "$c $arm $(echo "$e - $s" | bc)"
