#!/usr/bin/env bash
# arms.sh : fresh arms for every corpus with base and head-4
set -u
R=/var/tmp/d370-review2/arms
for arm in base:/var/tmp/d370/bin/base-6f4e17de head:/var/tmp/d370/bin/head-4; do
  a=${arm%%:*}; bin=${arm#*:}
  for c in std flask hono leveldb cobra gin; do
    d="$R/$a/$c"; rm -rf "${d:?}"; mkdir -p "$R/$a"; cp -a /var/tmp/d370/corpora/$c "$d"
    ( cd "$d" && HOME=/var/tmp/d370-review2/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 "$bin" incremental-index --no-embed > /dev/null 2> "$R/$a.$c.log"; echo "$a $c exit $?" )
  done
done
