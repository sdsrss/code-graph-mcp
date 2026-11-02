#!/usr/bin/env bash
# run5.sh: full stdlib index, base vs head-5, 3 interleaved runs each (user/sys/real)
W=/var/tmp/d370; TIMEFORMAT='%U %S %R'
export HOME=/var/tmp/d370/parity/fxhome CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1
for i in 1 2 3 4 5; do
  for arm in base-6f4e17de head-5; do
    D="$W/perf/std-$arm"; rm -rf "${D:?}"; cp -a "$W/corpora/std" "$D"
    t=$( { time (cd "$D" && "$W/bin/$arm" incremental-index --no-embed >/dev/null 2>&1); } 2>&1 )
    echo "$arm run$i user/sys/real $t"
  done
done
