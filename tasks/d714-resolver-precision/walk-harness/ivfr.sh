#!/bin/bash
# ivfr.sh BIN CASEDIR: like ivfm.sh, but the edit reaches the index through the query-time
# refresh (apply_file_refreshes, the ensure_file_indexed path): `deps` for each deleted file,
# then one `affected` naming every changed or new file. Compares edges with metadata.
set -e
BIN="$1"; C="$2"; H="$(dirname "$0")"
W="${C:?}/wr"; F="${C:?}/fr"
rm -rf "${W:?}" "${F:?}"
cp -a "$C/before" "$W"; (cd "$W" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
(cd "$W" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
deleted=(); changed=()
for f in $(cd "$W" && git ls-files); do [ -e "$C/after/$f" ] || deleted+=("$f"); done
for f in $(cd "$C/after" && find . -type f -not -path './.git/*' | sed 's|^\./||'); do
  if ! cmp -s "$C/after/$f" "$W/$f" 2>/dev/null; then changed+=("$f"); fi
done
for f in "${deleted[@]}"; do rm -f "${W:?}/$f"; done
cp -a "$C/after/." "$W/"
for f in "${deleted[@]}"; do (cd "$W" && "$BIN" deps "$f" >/dev/null 2>&1 || true); done
if [ ${#changed[@]} -gt 0 ]; then (cd "$W" && "$BIN" affected "${changed[@]}" >/dev/null 2>&1 || true); fi
cp -a "$C/after" "$F"; (cd "$F" && git init -q && git add -A && git -c user.email=a@b -c user.name=a commit -qm i)
(cd "$F" && "$BIN" incremental-index --no-embed --quiet >/dev/null 2>&1)
"$H/edgesm.sh" "$W" > "$C/incr.txt"
"$H/edgesm.sh" "$F" > "$C/fullr.txt"
if diff -q "$C/incr.txt" "$C/fullr.txt" >/dev/null; then echo "SAME ($(wc -l < "$C/fullr.txt") edges) del=${#deleted[@]} chg=${#changed[@]}"; else echo "DIFF: del=${#deleted[@]} chg=${#changed[@]}"; diff "$C/incr.txt" "$C/fullr.txt" | grep '^[<>]' | sed 's/^</  incremental-only:/; s/^>/  rebuild-only:/'; fi
