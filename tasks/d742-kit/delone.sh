#!/bin/bash
# delone.sh BIN CORPUS FILE: index CORPUS copy, delete FILE, incremental; compare with a fresh index
BIN="$1"; C="$2"; F="$3"; H=/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness
X="${W:?}/walk/delone-$(basename "$BIN")"; Y="$X-full"
rm -rf "${X:?}" "${Y:?}"; cp -a "$C" "$X"; rm -rf "${X:?}/.code-graph"
(cd "$X" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
rm "$X/$F"; (cd "$X" && git add -A && git -c user.email=a@b -c user.name=a commit -qm e >/dev/null 2>&1; "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
cp -a "$X" "$Y"; rm -rf "${Y:?}/.code-graph"; (cd "$Y" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
pend() { sqlite3 -readonly "file:$1/.code-graph/index.db?mode=ro" "SELECT COALESCE(f.path,'?')||':'||n.name||' => '||p.target_name||' '||COALESCE(p.metadata,'') FROM pending_unresolved_calls p JOIN nodes n ON n.id=p.source_id LEFT JOIN files f ON f.id=n.file_id" | sort | sed 's/^/PENDING /'; }
{ "$H/edgesm.sh" "$X"; pend "$X"; } > "$X.txt"; { "$H/edgesm.sh" "$Y"; pend "$Y"; } > "$Y.txt"
echo "$(basename "$BIN"): incremental-only $(diff "$X.txt" "$Y.txt" | grep -c '^<') rebuild-only $(diff "$X.txt" "$Y.txt" | grep -c '^>')"
diff "$X.txt" "$Y.txt" | grep '^[<>]' | head -4
rm -rf "${X:?}" "${Y:?}"
