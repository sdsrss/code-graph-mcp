#!/usr/bin/env bash
# cmp.sh CASEDIR : index CASEDIR's *.py with 0.182.0 (old), 5e239e4a (base, if built) and e3088926 (head); dump call edges; run each file with python3
set -u
C="${1:?}"; R=/var/tmp/d370g-review2
declare -A BINS=([old]=/var/tmp/d370g/bin/6f4e17de [head]=$R/bin/e3088926)
[ -x "$R/bin/5e239e4a" ] && BINS[base]=$R/bin/5e239e4a
for arm in old base head; do
  B="${BINS[$arm]:-}"; [ -n "$B" ] || continue
  W="$R/work/$(basename "$C")-$arm"; rm -rf "${W:?}"; mkdir -p "$W"
  (cd "$C" && tar --exclude=.code-graph --exclude=.git --exclude=__pycache__ -cf - .) | tar -C "$W" -xf -
  (cd "$W" && git init -q && HOME=$R/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 "$B" incremental-index --no-embed >/dev/null 2>"$W.err"; echo "[$arm exit $?]")
  python3 -I $R/tools/dump.py "$W/.code-graph/index.db" > "$W.edges" 2>&1
done
for arm in old base head; do [ -f "$R/work/$(basename "$C")-$arm.edges" ] && { echo "== $arm"; grep -v '^P' "$R/work/$(basename "$C")-$arm.edges" | sed 's/^/  /'; grep -c '^P' "$R/work/$(basename "$C")-$arm.edges" | sed 's/^/  pending: /'; }; done
