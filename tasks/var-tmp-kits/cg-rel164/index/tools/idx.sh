#!/usr/bin/env bash
# idx.sh ARM DIR  -> full/incremental index DIR with bin/ARM; prints json line
set -uo pipefail
BIN=/var/tmp/cg-rel164/index/bin/$1
cd "$2" || exit 2
HOME=/var/tmp/cg-rel164/index/home CODE_GRAPH_NO_GITIGNORE=1 CODE_GRAPH_NO_AUTO_ADOPT=1 "$BIN" incremental-index --no-embed --json 2>>/var/tmp/cg-rel164/index/idx-stderr.log
echo "exit=$?" >&2
