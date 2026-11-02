#!/usr/bin/env bash
set -u
R=/var/tmp/d370-review3
for arm in base:6f4e17de head:001b74a9 prev:a653b1a1; do
  a=${arm%%:*}; b=${arm#*:}
  CORPORA="std:python" "$R/tools/score.sh" "$a" "$R/bin/$b" > "$R/out/score-$a.log" 2>&1
  for name in flask hono leveldb gin cobra; do
    d="$R/arms/$a/$name"; [ -d "$d" ] || cp -a "/var/tmp/d370/corpora/$name" "$d"
    ( cd "$d" && HOME=$R/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 "$R/bin/$b" incremental-index --no-embed >"$R/out/$a-$name.index.log" 2>&1; echo "$a $name exit $?" )
  done
done
echo arms-done
