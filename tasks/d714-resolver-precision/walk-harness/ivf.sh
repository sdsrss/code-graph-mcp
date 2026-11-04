#!/bin/bash
# usage: incr_vs_full.sh BIN CASEDIR
# CASEDIR/before/ = initial tree, CASEDIR/after/ = final tree (files absent in after are deleted)
set -e
BIN="$1"; C="$2"
W="${C:?}/w"; F="${C:?}/f"
rm -rf "${W:?}" "${F:?}"
cp -a "$C/before" "$W"; (cd "$W" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
(cd "$W" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
# apply after: delete files missing from after, copy after over
(cd "$W" && for f in $(git ls-files); do [ -e "$C/after/$f" ] || rm -f "$f"; done)
cp -a "$C/after/." "$W/"
(cd "$W" && git add -A && git -c user.email=a@b -c user.name=a commit -qm e >/dev/null 2>&1 || true)
(cd "$W" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
cp -a "$C/after" "$F"; (cd "$F" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
(cd "$F" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
"$(dirname "$0")/edges.sh" "$W" > "$C/inc.txt"
"$(dirname "$0")/edges.sh" "$F" > "$C/full.txt"
if diff -q "$C/inc.txt" "$C/full.txt" >/dev/null; then echo "SAME ($(wc -l < "$C/full.txt") edges)"; else echo "DIFF:"; diff "$C/inc.txt" "$C/full.txt" | grep '^[<>]' | sed 's/^</  incremental-only:/; s/^>/  rebuild-only:/'; fi
