#!/usr/bin/env bash
# timeit.sh <case-dir> <runs> : index a fresh copy per run with each arm (alternating), print wall seconds
set -u
R=/var/tmp/d905-review
case="$1"; runs="$2"; name=$(basename "$case")
export CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1
for i in $(seq 1 "$runs"); do
  for arm in base head; do
    W="${R:?}/tw/$name-$arm"
    rm -rf "${W:?}"; mkdir -p "$W"; cp -a "$case/." "$W/"
    (cd "$W" && git init -q && git add -A && git -c user.email=a@b -c user.name=r commit -qm x)
    s=$(date +%s%N)
    (cd "$W" && timeout 600 "$R/$arm-bin" incremental-index --no-embed --quiet >/dev/null 2>&1); rc=$?
    e=$(date +%s%N)
    echo "$name $arm run$i $(( (e - s) / 1000000 )) ms rc=$rc"
  done
done
