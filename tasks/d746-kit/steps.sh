#!/bin/bash
# steps.sh ARM SET...: one-step chains (s_{k-1} indexed fresh, s_k applied) under sets-step/, 8 in parallel
W=/var/tmp/agent-tmp-d746; H=/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness; A="$1"; shift; B="$W/bin/$A"
for s in "$@"; do
  R="$W/run-step-$A/$s"; O="$W/out/steps-$A"; rm -rf "${R:?}"; mkdir -p "$R" "$O"; cp -a "$W/sets-step/$s/." "$R/"
  ls "$R" | xargs -P 8 -I{} sh -c 'r=$('"$H"'/ivchain.sh '"$B"' '"$R"'/{} 2>&1); echo "{} $(echo "$r" | head -1)"; echo "$r" | tail -n +2 | sed "s/^/{} /" >> '"$O/$s"'.detail' | sort > "$O/$s.txt"
  echo "$A $s: $(grep -c DIFF "$O/$s.txt") DIFF / $(wc -l < "$O/$s.txt")"
  rm -rf "${R:?}"
done
