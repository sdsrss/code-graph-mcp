#!/bin/bash
# whole3.sh ARM: whole chains of sets3 (g102, g103), 8 in parallel
W=/var/tmp/agent-tmp-d746; H=/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness; A="$1"; B="$W/bin/$A"
for s in g102 g103; do R="$W/run-whole3-$A/$s"; O="$W/out/whole3-$A"; rm -rf "${R:?}"; mkdir -p "$R" "$O"; cp -a "$W/sets3/$s/." "$R/"
  ls "$R" | xargs -P 8 -I{} sh -c 'echo "{} $('"$H"'/ivchain.sh '"$B"' '"$R"'/{} 2>&1 | head -1)"' | sort > "$O/$s.txt"
  echo "$A whole $s: $(grep -c DIFF "$O/$s.txt") DIFF / $(wc -l < "$O/$s.txt")"; rm -rf "${R:?}"; done
