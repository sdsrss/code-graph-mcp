#!/bin/bash
# loop_full.sh ARM SRC NAME N : N full indexes of one tree copy, counting failures
ARM=$1; SRC=$2; NAME=$3; N=$4
D=/var/tmp/d175-r2/corp/loop-$NAME-$ARM
/var/tmp/d175-r2/harness/cpcorp.sh "$SRC" "$D"
export CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1
fails=0
for i in $(seq 1 "$N"); do
  rm -rf "${D:?}/.code-graph"
  out=$(cd "$D" && /var/tmp/d175-r2/bin/$ARM incremental-index --no-embed --quiet 2>&1 >/dev/null); rc=$?
  if [ $rc -ne 0 ]; then
    fails=$((fails+1))
    echo "run $i rc=$rc: $(echo "$out" | grep -v WARN | tail -1)"
    cp -r "$D/.code-graph" "/var/tmp/d175-r2/ki/loop-$NAME-$ARM-$i"
  fi
done
echo "$NAME $ARM: $fails failures in $N full indexes"
rm -rf "${D:?}"
