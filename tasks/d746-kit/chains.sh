#!/bin/bash
# chains.sh ARM: every chain set and kit under $W/sets with bin/ARM, 8 in parallel; counts + ids in out/chains-ARM/
W=/var/tmp/agent-tmp-d746; H=/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness; A="$1"; B="$W/bin/$A"
R="$W/run-$A"; O="$W/out/chains-$A"; rm -rf "${R:?}" "${O:?}"; mkdir -p "$R" "$O"; cp -a "$W/sets/." "$R/"
for s in s21 s22 fam1 fam2; do
  ls "$R/$s" | xargs -P 8 -I{} sh -c 'echo "{} $('"$H"'/ivchain.sh '"$B"' '"$R/$s"'/{} 2>&1 | head -1)"' | sort > "$O/$s.txt"
  echo "$A $s: $(grep -c DIFF "$O/$s.txt") DIFF / $(wc -l < "$O/$s.txt")"
done
for k in kit38 r4sets61; do
  ls "$R/kits/$k" | xargs -P 8 -I{} sh -c 'echo "{} $('"$H"'/ivfm.sh '"$B"' '"$R/kits/$k"'/{} 2>&1 | tail -1)"' | sort > "$O/$k.txt"
  echo "$A $k: $(grep -c ' SAME' "$O/$k.txt")/$(wc -l < "$O/$k.txt") SAME"
done
rm -rf "${R:?}"
