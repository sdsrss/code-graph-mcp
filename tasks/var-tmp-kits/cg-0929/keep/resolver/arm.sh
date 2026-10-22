#!/usr/bin/env bash
# Index every corpus with one binary and score it against the shared SCIP gold.
#   arm.sh ARM BIN [corpus...]  -> /var/tmp/cg-0929/resolver/arms/ARM/{CORPUS/, out/}
set -uo pipefail
R=/var/tmp/cg-0929/resolver
ARM="$1"; BIN="$2"; shift 2
CORPORA=("${@:-flask hono express tokio self}")
[ $# -eq 0 ] && CORPORA=(flask hono express tokio self)
A="$R/arms/$ARM"; mkdir -p "$A/out"
md5sum "$BIN" > "$A/out/binary.md5"
langs() { case "$1" in flask|networkx) echo python;; hono|express) echo javascript;; tokio) echo rust;; self) echo "rust javascript python";; esac; }
for c in "${CORPORA[@]}"; do
  rm -rf "${A:?}/$c"; cp -a "$R/corpora/$c" "$A/$c"
  ( cd "$A/$c" && "$BIN" incremental-index --quiet ) >"$A/out/idx-$c.log" 2>&1 || echo "index $c failed: $?" >> "$A/out/idx-$c.log"
  for l in $(langs "$c"); do
    S="$R/scip/$c-$l"
    root="$S/src"; [ "$l" = rust ] && root="$A/$c"
    python3 "$R/wt/scripts/scip_oracle/oracle.py" --language "$l" --scip "$S/index.scip" --root "$root" \
      --db "$A/$c/.code-graph/index.db" --samples 40 \
      --json-out "$A/out/$c-$l.json" --dump-judged "$A/out/$c-$l-judged.json" > "$A/out/$c-$l.txt" 2>&1
    echo "$c $l exit=$?"
  done
done
