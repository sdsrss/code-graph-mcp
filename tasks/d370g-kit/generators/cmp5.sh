#!/usr/bin/env bash
# cmp5.sh SRC_CASE_DIR NAME : copy, index with base/head-4/head-5, dump call edges per arm
set -u
SP=/tmp/claude-1000/-home-ai-dev-code-graph-mcp/4b3190a7-0e99-487e-873c-e9645c3a3f8b/scratchpad
W="${SP:?}/cases5"; SRC="$1"; N="$2"
for b in base-6f4e17de head-4 head-5; do
  D="${W:?}/$N.$b"; rm -rf "${D:?}"; mkdir -p "$D"
  (cd "$SRC" && tar --exclude=.code-graph --exclude=.git -cf - .) | tar -C "$D" -xf -
  (cd "$D" && git init -q && HOME="${SP:?}/f1/home" CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 /var/tmp/d370/bin/$b incremental-index --no-embed >/dev/null 2>&1) || echo "INDEX FAIL $N $b"
  python3 -I /var/tmp/d370-review2/tools/edges.py "$D" | grep -v "^PENDING" | sed 's/ \[[^]]*\]$//' | sort -u > "$W/$N.$b.edges"
done
