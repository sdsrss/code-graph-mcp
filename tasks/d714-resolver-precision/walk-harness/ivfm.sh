#!/bin/bash
# like ivf.sh but compares edges WITH metadata and node types; also pending table
set -e
BIN="$1"; C="$2"; H="$(dirname "$0")"
W="${C:?}/w"; F="${C:?}/f"
rm -rf "${W:?}" "${F:?}"
cp -a "$C/before" "$W"; (cd "$W" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
(cd "$W" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
(cd "$W" && for f in $(git ls-files); do [ -e "$C/after/$f" ] || rm -f "$f"; done)
cp -a "$C/after/." "$W/"
(cd "$W" && git add -A && git -c user.email=a@b -c user.name=a commit -qm e >/dev/null 2>&1 || true)
(cd "$W" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
cp -a "$C/after" "$F"; (cd "$F" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
(cd "$F" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
"$H/edgesm.sh" "$W" > "$C/incm.txt"
"$H/edgesm.sh" "$F" > "$C/fullm.txt"
if diff -q "$C/incm.txt" "$C/fullm.txt" >/dev/null; then echo "SAME ($(wc -l < "$C/fullm.txt") edges)"; else echo "DIFF:"; diff "$C/incm.txt" "$C/fullm.txt" | grep '^[<>]' | sed 's/^</  incremental-only:/; s/^>/  rebuild-only:/'; fi
