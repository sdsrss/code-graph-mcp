#!/usr/bin/env bash
# timeab.sh <base-bin> <head-bin> <work> <runs> <case-dir>... : fresh copy per run, arms alternating, wall ms
set -u
B="$1"; H="$2"; WK="${3:?}"; N="$4"; shift 4
export CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1
for c in "$@"; do
  name=$(basename "$c")
  for i in $(seq 1 "$N"); do
    for arm in base head; do
      bin="$B"; [ "$arm" = head ] && bin="$H"
      W="${WK:?}/$name-$arm"; rm -rf "${W:?}"; mkdir -p "$W"
      (cd "$c" && tar --exclude=.code-graph --exclude=.git -cf - .) | (cd "$W" && tar -xf -)
      (cd "$W" && git init -q && git add -A && git -c user.email=a@b -c user.name=r commit -qm x)
      s=$(date +%s%N); (cd "$W" && timeout 900 "$bin" incremental-index --no-embed --quiet >/dev/null 2>&1); rc=$?; e=$(date +%s%N)
      echo "$name $arm $(( (e - s) / 1000000 )) rc=$rc"
      rm -rf "${W:?}"
    done
  done
done
