#!/bin/bash
# timing3.sh A B: hono + the reviewer's adversarial trees (fanin, sibonly, freefn), full index and edits, arms alternated, 3 runs each
W=/var/tmp/agent-tmp-d746; A="$1"; B="$2"; P=$W/perf
ms() { local s e; s=$(date +%s%N); (cd "$1" && "$W/bin/$2" incremental-index --no-embed --quiet >/dev/null 2>&1); e=$(date +%s%N); echo $(( (e-s)/1000000 )); }
for c in hono fanin sibonly freefn; do
  src=$P/$c; [ "$c" = hono ] && src=$W/corpora/hono
  for arm in $A $B; do rm -rf "${P:?}/t3-$c-$arm"; cp -a "$src" "$P/t3-$c-$arm"; done
  line="$c full"
  for i in 1 2 3; do for arm in $A $B; do rm -rf "${P:?}/t3-$c-$arm/.code-graph"; line="$line $arm=$(ms "$P/t3-$c-$arm" $arm)"; done; done
  echo "$line (load $(cut -d' ' -f1 /proc/loadavg))"
  case $c in hono) files="src/jsx/hooks/index.ts src/context.ts";; fanin) files="lib/m.ts app/f1.tsx";; sibonly) files="app/f1.tsx lib/g.js";; freefn) files="app/f1.js lib/h.js";; esac
  for f in $files; do
    line="$c edit $f"
    for i in 1 2 3; do for arm in $A $B; do echo "// t$i" >> "$P/t3-$c-$arm/$f"; line="$line $arm=$(ms "$P/t3-$c-$arm" $arm)"; done; done
    echo "$line (load $(cut -d' ' -f1 /proc/loadavg))"
  done
done
