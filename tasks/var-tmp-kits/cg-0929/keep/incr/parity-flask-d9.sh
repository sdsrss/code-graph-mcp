#!/usr/bin/env bash
# parity-flask-d9.sh <bin> <tag>: index flask without src/flask/helpers.py, restore it,
# index incrementally; compare every edge with a fresh full index of the same tree.
set -u
BIN=$1; TAG=$2; W=/var/tmp/cg-0929/incr/par/$TAG; P=/var/tmp/cg-0929/incr/pristine/flask
rm -rf "${W:?}"; mkdir -p "$W"
cp -a "$P" "$W/inc"; cp -a "$P" "$W/full"
mv "$W/inc/src/flask/helpers.py" "$W/helpers.py.aside"
(cd "$W/inc" && "$BIN" incremental-index --no-embed >/dev/null 2>&1)
mv "$W/helpers.py.aside" "$W/inc/src/flask/helpers.py"
(cd "$W/inc" && "$BIN" incremental-index --no-embed >/dev/null 2>&1)
(cd "$W/full" && "$BIN" incremental-index --no-embed >/dev/null 2>&1)
/var/tmp/cg-0929/incr/dump.sh "$W/inc/.code-graph/index.db" > "$W/inc.edges"
/var/tmp/cg-0929/incr/dump.sh "$W/full/.code-graph/index.db" > "$W/full.edges"
echo "$TAG: inc=$(wc -l < "$W/inc.edges") full=$(wc -l < "$W/full.edges") only_inc=$(comm -23 "$W/inc.edges" "$W/full.edges" | wc -l) only_full=$(comm -13 "$W/inc.edges" "$W/full.edges" | wc -l)"
