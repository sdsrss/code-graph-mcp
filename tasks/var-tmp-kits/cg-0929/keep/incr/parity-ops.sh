#!/usr/bin/env bash
# parity-ops.sh <bin> <tag> <corpus> <ops-file>: full-index <corpus>, apply the
# shell ops in <ops-file> (run with cwd = tree), index incrementally; compare every
# edge with a fresh full index of a copy that had the same ops applied first.
set -u
BIN=$1; TAG=$2; P=/var/tmp/cg-0929/incr/pristine/$3; OPS=$4; W=/var/tmp/cg-0929/incr/par/$TAG
rm -rf "${W:?}"; mkdir -p "$W"
cp -a "$P" "$W/inc"; cp -a "$P" "$W/full"
(cd "$W/inc" && "$BIN" incremental-index --no-embed >/dev/null 2>&1)
(cd "$W/inc" && bash "$OPS")
(cd "$W/inc" && "$BIN" incremental-index --no-embed >/dev/null 2>&1)
(cd "$W/full" && bash "$OPS" && "$BIN" incremental-index --no-embed >/dev/null 2>&1)
/var/tmp/cg-0929/incr/dump.sh "$W/inc/.code-graph/index.db" > "$W/inc.edges"
/var/tmp/cg-0929/incr/dump.sh "$W/full/.code-graph/index.db" > "$W/full.edges"
echo "$TAG: inc=$(wc -l < "$W/inc.edges") full=$(wc -l < "$W/full.edges") only_inc=$(comm -23 "$W/inc.edges" "$W/full.edges" | wc -l) only_full=$(comm -13 "$W/inc.edges" "$W/full.edges" | wc -l)"
