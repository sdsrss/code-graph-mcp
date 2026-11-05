#!/bin/bash
# ivchain.sh BIN CASEDIR: CASEDIR/s0 .. sN are successive trees. s0 indexed, each next state applied
# incrementally to the same index; after each step the edges (with metadata) and the pending-call rows
# are compared with a fresh index of that state. Prints SAME or the first differing step.
set -e
BIN="$1"; C="$2"; H="$(dirname "$0")"
W="${C:?}/w"; F="${C:?}/f"
rm -rf "${W:?}"
mkdir -p "$W"; cp -a "$C/s0/." "$W/"; (cd "$W" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
(cd "$W" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
pend() { sqlite3 -readonly "file:$1/.code-graph/index.db?mode=ro" "SELECT COALESCE(f.path,'?')||':'||n.name||' => '||p.target_name||' '||COALESCE(p.metadata,'') FROM pending_unresolved_calls p JOIN nodes n ON n.id=p.source_id LEFT JOIN files f ON f.id=n.file_id" 2>/dev/null | sort; }
i=1; out=""
while [ -d "$C/s$i" ]; do
  (cd "$W" && for f in $(git ls-files); do [ -e "$C/s$i/$f" ] || rm -f "$f"; done)
  cp -a "$C/s$i/." "$W/"
  (cd "$W" && git add -A && git -c user.email=a@b -c user.name=a commit -qm e >/dev/null 2>&1 || true)
  (cd "$W" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
  rm -rf "${F:?}"; mkdir -p "$F"; cp -a "$C/s$i/." "$F/"; (cd "$F" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
  (cd "$F" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
  "$H/edgesm.sh" "$W" > "$C/inc$i.txt"; pend "$W" | sed 's/^/PENDING /' >> "$C/inc$i.txt"
  "$H/edgesm.sh" "$F" > "$C/full$i.txt"; pend "$F" | sed 's/^/PENDING /' >> "$C/full$i.txt"
  if ! diff -q "$C/inc$i.txt" "$C/full$i.txt" >/dev/null; then
    echo "DIFF at step $i:"; diff "$C/inc$i.txt" "$C/full$i.txt" | grep '^[<>]' | sed 's/^</  incremental-only:/; s/^>/  rebuild-only:/'; exit 0
  fi
  i=$((i+1))
done
echo "SAME ($((i-1)) steps)"
