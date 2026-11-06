#!/bin/bash
# timing2.sh A B: full index (hono, bigjsg) and single-file edits, arms alternated, 3 runs each; ms per run
W=/var/tmp/agent-tmp-d746; A="$1"; B="$2"; P=$W/perf
ms() { local s e; s=$(date +%s%N); (cd "$1" && "$W/bin/$2" incremental-index --no-embed --quiet >/dev/null 2>&1); e=$(date +%s%N); echo $(( (e-s)/1000000 )); }
for c in hono bigjsg claude-mem-lite daagu; do
  src=$W/corpora/$c; [ "$c" = bigjsg ] && src=$P/bigjsg
  for arm in $A $B; do rm -rf "${P:?}/$c-$arm"; cp -a "$src" "$P/$c-$arm"; done
  line="$c full"
  for i in 1 2 3; do for arm in $A $B; do rm -rf "${P:?}/$c-$arm/.code-graph"; line="$line $arm=$(ms "$P/$c-$arm" $arm)"; done; done
  echo "$line (load $(cut -d' ' -f1 /proc/loadavg))"
  case $c in hono) files="src/jsx/hooks/index.ts src/context.ts";; bigjsg) files="lib/m0.ts app/f1.tsx";; *) files="$(cd "$src" && git ls-files '*.mjs' '*.ts' '*.js' | head -1)";; esac
  for f in $files; do
    line="$c edit $f"
    for i in 1 2 3; do for arm in $A $B; do echo "// t$i" >> "$P/$c-$arm/$f"; line="$line $arm=$(ms "$P/$c-$arm" $arm)"; done; done
    echo "$line (load $(cut -d' ' -f1 /proc/loadavg))"
  done
done
