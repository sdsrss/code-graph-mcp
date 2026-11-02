#!/usr/bin/env bash
# idx.sh BIN DIR : fresh full index of DIR with BIN
set -u
BIN="$1"; D="$2"
rm -rf "${D:?}/.code-graph"
cd "$D" && { [ -d .git ] || git init -q; } && HOME=/var/tmp/d370-review2/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 "$BIN" incremental-index --no-embed --json > /dev/null 2> /var/tmp/d370-review2/idx.err || { echo "INDEX FAIL"; tail -5 /var/tmp/d370-review2/idx.err; }
