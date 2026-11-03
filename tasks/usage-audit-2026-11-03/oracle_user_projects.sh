#!/usr/bin/env bash
# Score code-graph call edges on fresh git-archive copies of the user's projects (2026-11-03 audit),
# indexed by the current binary without the embedding model (edges do not depend on it).
set -uo pipefail
SP="${1:?scratch dir}"
W=/var/tmp/cg-audit-1103
mkdir -p "$W"
export SCIP_TOOLS_BIN=/var/tmp/scip-tools/node_modules/.bin
export CODE_GRAPH_MODEL_DIR="$W/no-model" CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1
mkdir -p "$CODE_GRAPH_MODEL_DIR"
ORACLE=/home/ai/dev/code-graph-mcp/scripts/scip_oracle/run.sh
for spec in daagu:python daagu:javascript gsd-lite:javascript claudemd:javascript sgc:javascript claude-mem-lite:javascript; do
  p="${spec%%:*}"; lang="${spec##*:}"; d="$W/$p"
  if [ ! -d "$d/.git" ]; then
    mkdir -p "$d" && git -C "/home/ai/dev/$p" archive HEAD | tar -x -C "$d"
    (cd "$d" && git init -q . && git add -A && git -c user.name=a -c user.email=a@a commit -qm snap)
  fi
  if [ ! -f "$d/.code-graph/index.db" ]; then
    s=$(date +%s.%N)
    (cd "$d" && code-graph-mcp incremental-index --no-embed --json > "$SP/oracle/$p.index.json" 2> "$SP/oracle/$p.index.err"); echo "$p index rc=$? $(python3 -c "import time;print(round(time.time()-$s,1))")s" >> "$SP/oracle/summary.txt"
  fi
  "$ORACLE" --repo "$d" --language "$lang" --json-out "$SP/oracle/$p-$lang.json" --samples 40 > "$SP/oracle/$p-$lang.txt" 2>&1
  echo "$p $lang oracle rc=$?" >> "$SP/oracle/summary.txt"
done
echo DONE >> "$SP/oracle/summary.txt"
