#!/usr/bin/env bash
# Build base snapshots (parent of each task commit) and a code-graph index for the plugin arm.
# usage: setup.sh OUT_DIR   (OUT_DIR/base/<task>, OUT_DIR/base-indexed/<task>)
set -euo pipefail
OUT="${1:?out dir}"
REPO=/home/ai/dev/daagu
CG="$HOME/.cache/code-graph/bin/code-graph-mcp"
HERE="$(cd "$(dirname "$0")" && pwd)"
# task -> commit, from the git-excluded private/tasks.json
while read -r t c; do
  b="$OUT/base/$t"; bi="$OUT/base-indexed/$t"
  [ -d "$b" ] && continue
  mkdir -p "$b"
  git -C "$REPO" archive "$c^" | tar -x -C "$b"
  (cd "$b" && git init -q && git add -A && git -c user.name=eval -c user.email=eval@localhost commit -q -m "snapshot")
  mkdir -p "$(dirname "$bi")"
  cp -a "$b" "$bi"
  (cd "$bi" && CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1 "$CG" incremental-index --no-embed --json | tail -1)
  du -sh "$b" "$bi/.code-graph"
done < <(python3 -c "import json,sys; [print(k, v['commit']) for k, v in json.load(open(sys.argv[1])).items()]" "$HERE/private/tasks.json")
