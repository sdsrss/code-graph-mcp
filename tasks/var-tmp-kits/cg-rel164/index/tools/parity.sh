#!/usr/bin/env bash
# parity.sh ARM CORPUS STEPFILE TAG
# Copies pristine CORPUS, full-indexes it with bin/ARM, then for each step_N in STEPFILE:
# applies it, runs incremental-index, and compares against a fresh full index of the same tree.
set -uo pipefail
ARM=$1; CORPUS=$2; STEPS=$3; TAG=$4
R=/var/tmp/cg-rel164/index/parity/$TAG/$ARM; T=/var/tmp/cg-rel164/index/tools
rm -rf "${R:?}"; mkdir -p "$R"
SRC=/var/tmp/cg-rel164/index/corp/pristine/$CORPUS; [ -d "$SRC" ] || SRC=/var/tmp/cg-rel164/index/fx/$CORPUS
cp -a "$SRC" "$R/work"; rm -rf "${R:?}/work/.code-graph"
$T/idx.sh $ARM "$R/work" > "$R/step0.json" 2>/dev/null
source "$STEPS"
export LC_ALL=C
n=1
while declare -F step_$n >/dev/null; do
  (cd "$R/work" && step_$n)
  $T/idx.sh $ARM "$R/work" > "$R/step$n.incr.json" 2>/dev/null
  rm -rf "${R:?}/fresh"; mkdir -p "$R/fresh"
  (cd "$R/work" && tar --exclude=./.code-graph -cf - .) | (cd "$R/fresh" && tar -xf -)
  $T/idx.sh $ARM "$R/fresh" > "$R/step$n.full.json" 2>/dev/null
  python3 $T/dump.py "$R/work/.code-graph/index.db" "$R/s$n.incr" >/dev/null
  python3 $T/dump.py "$R/fresh/.code-graph/index.db" "$R/s$n.full" >/dev/null
  ne=$(comm -3 "$R/s$n.incr.edges" "$R/s$n.full.edges" | wc -l)
  nn=$(comm -3 "$R/s$n.incr.nodes" "$R/s$n.full.nodes" | wc -l)
  ie=$(comm -23 "$R/s$n.incr.edges" "$R/s$n.full.edges" | wc -l); fe=$(comm -13 "$R/s$n.incr.edges" "$R/s$n.full.edges" | wc -l)
  echo "$TAG $ARM step$n: nodes-diff=$nn edges only-incr=$ie only-full=$fe $(cat $R/step$n.incr.json | python3 -c 'import json,sys; j=json.load(sys.stdin); print("incr_files=%s mode=%s"%(j.get("files_indexed"),j.get("mode")))')"
  comm -23 "$R/s$n.incr.edges" "$R/s$n.full.edges" > "$R/s$n.only-incr"; comm -13 "$R/s$n.incr.edges" "$R/s$n.full.edges" > "$R/s$n.only-full"
  n=$((n+1))
done
