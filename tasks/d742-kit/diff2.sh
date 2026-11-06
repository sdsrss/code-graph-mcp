#!/bin/bash
# diff2.sh W ARM_A ARM_B DIR...: index each corpus dir with bin/ARM_A and bin/ARM_B; compare edges (with metadata) + pending rows
W="$1"; A="$2"; B="$3"; shift 3; H=/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness
mkdir -p "$W/out"
for c in "$@"; do
  n=$(basename "$c")
  for arm in "$A" "$B"; do
    rm -rf "${c:?}/.code-graph"
    s=$(date +%s%N); (cd "$c" && "$W/bin/$arm" incremental-index --no-embed --quiet >/dev/null 2>&1); e=$(date +%s%N)
    "$H/edgesm.sh" "$c" > "$W/out/$n.$arm.txt"
    sqlite3 -readonly "file:$c/.code-graph/index.db?mode=ro" "SELECT COALESCE(f.path,'?')||':'||n.name||'@'||n.start_line||' => '||p.target_name||' '||COALESCE(p.metadata,'') FROM pending_unresolved_calls p JOIN nodes n ON n.id=p.source_id LEFT JOIN files f ON f.id=n.file_id" | sort | sed 's/^/PENDING /' >> "$W/out/$n.$arm.txt"
  done
  if cmp -s "$W/out/$n.$A.txt" "$W/out/$n.$B.txt"; then echo "$n SAME ($(wc -l < "$W/out/$n.$A.txt"))"; else echo "$n DIFF -$(diff "$W/out/$n.$A.txt" "$W/out/$n.$B.txt" | grep -c '^<') +$(diff "$W/out/$n.$A.txt" "$W/out/$n.$B.txt" | grep -c '^>')"; fi
done
