#!/usr/bin/env bash
# time3.sh <work> <runs> <case-dir>... : arms v0203/pre/rep, order rotated per round, fresh copy per run, wall ms
set -u
R=/var/tmp/d909-review2
WK="${1:?}"; N="$2"; shift 2
export CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1
ORDERS=("v0203 pre rep" "pre rep v0203" "rep v0203 pre")
for i in $(seq 1 "$N"); do
  for c in "$@"; do
    name=$(basename "$c")
    for arm in ${ORDERS[$(( (i - 1) % 3 ))]}; do
      W="${WK:?}/$name-$arm"; rm -rf "${W:?}"; mkdir -p "$W"
      (cd "$c" && tar --exclude=.code-graph --exclude=.git -cf - .) | (cd "$W" && tar -xf -)
      (cd "$W" && git init -q && git add -A && git -c user.email=a@b -c user.name=r commit -qm x)
      s=$(date +%s%N); (cd "$W" && timeout 900 "$R/bin/$arm" incremental-index --no-embed --quiet >/dev/null 2>&1); rc=$?; e=$(date +%s%N)
      echo "$name $arm $(( (e - s) / 1000000 )) rc=$rc"
      rm -rf "${W:?}"
    done
  done
done
