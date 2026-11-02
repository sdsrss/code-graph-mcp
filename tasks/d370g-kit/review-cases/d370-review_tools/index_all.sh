#!/usr/bin/env bash
set -u
cd /var/tmp/d370-review/corp
export HOME=/var/tmp/d370-review/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1
for c in std flask hono leveldb cobra gin; do
  for b in base head; do
    ( cd "$c-$b" && /var/tmp/d370-review/parity/bin/$b incremental-index --no-embed > ../$c-$b.log 2>&1 ); echo "$c $b exit=$?"
  done
done
