#!/bin/bash
# runwalk.sh ARM: chains s21+s22 (ivchain) and kit38+r4sets61 (ivfm) on a private copy, counts per set
W=/var/tmp/agent-tmp-d745; H=/home/ai/dev/code-graph-mcp/tasks/d714-resolver-precision/walk-harness; A="$1"; B="$W/bin/$A"
R="$W/walk-$A"; rm -rf "${R:?}"; mkdir -p "$R"; cp -a "$W/chains" "$R/"; cp -a "$W/kits" "$R/"
for s in s21 s22; do d=0; for c in "$R/chains/$s"/*/; do out=$(bash "$H/ivchain.sh" "$B" "${c%/}"); case "$out" in SAME*) ;; *) d=$((d+1)); echo "${c%/}: $out" | head -3 >> "$R/chain-diffs.txt";; esac; done; echo "$A chains $s: $d DIFF / 300"; done
for k in kit38 r4sets61; do n=0; s=0; for c in "$R/kits/$k"/*/; do n=$((n+1)); out=$(bash "$H/ivfm.sh" "$B" "${c%/}" 2>&1 | tail -1); case "$out" in SAME*) s=$((s+1));; *) echo "$k $(basename "$c"): $out" >> "$R/kit-diffs.txt";; esac; done; echo "$A $k: $s/$n SAME"; done
