#!/usr/bin/env bash
# timeab3.sh <work> <runs> <case-dir>... : arms in $ARMS alternating, fresh copy per run, wall ms + edge count
set -u
R=/var/tmp/agent-tmp-d909-review3
WK="${1:?}"; N="$2"; shift 2
ARMS=${ARMS:-"v0.203.0 c52b115c f06f5e96"}
export CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1
for c in "$@"; do
  name=$(basename "$c")
  for i in $(seq 1 "$N"); do
    for arm in $ARMS; do
      D="${WK:?}/$name-$arm"; rm -rf "${D:?}"; mkdir -p "$D"
      (cd "$c" && tar --exclude=.code-graph --exclude=.git -cf - .) | (cd "$D" && tar -xf -)
      (cd "$D" && git init -q && git add -A && git -c user.email=a@b -c user.name=r commit -qm x)
      s=$(date +%s%N); (cd "$D" && timeout 900 "$R/bin/$arm" incremental-index --no-embed --quiet >/dev/null 2>&1); rc=$?; e=$(date +%s%N)
      ed=$(sqlite3 "$D/.code-graph/index.db" "select count(*) from edges where relation in ('calls','references')" 2>/dev/null)
      echo "$name $arm run$i $(( (e - s) / 1000000 )) ms rc=$rc edges=$ed"
      rm -rf "${D:?}"
    done
  done
done
