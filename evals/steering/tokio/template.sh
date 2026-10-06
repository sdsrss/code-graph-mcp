#!/usr/bin/env bash
# The workspace every tokio case starts from: tokio 1.41.1 as a one-commit git
# repo, indexed by the binary under test, as a user's project would already be.
#   template.sh [DIR]     (default $CG_STEER_WORK_DIR/tokio, /var/tmp/cg-steer/tokio;
#                          ab.py reads DIR/tokio)
# TOKIO_SRC: a tokio clone to archive from (default: cloned into DIR/clone).
# CG_EVAL_BINARY: the code-graph-mcp to index with (default target/release).
# The index is built under a scratch HOME with no embedding model, as in every
# eval session (FTS5 only); a copy of DIR/tokio stays fresh at any path.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/../../.." && pwd)"
DIR="${1:-${CG_STEER_WORK_DIR:-/var/tmp/cg-steer}/tokio}"
COMMIT=bb7ca7507b94d01ffe0e275ddc669734ab3bf783  # tag tokio-1.41.1
BIN="${CG_EVAL_BINARY:-$REPO/target/release/code-graph-mcp}"
mkdir -p "$DIR"
SRC="${TOKIO_SRC:-$DIR/clone}"
if [ ! -d "$SRC" ]; then
  git -c advice.detachedHead=false clone -q --branch tokio-1.41.1 https://github.com/tokio-rs/tokio.git "$SRC"
fi
[ "$(git -C "$SRC" rev-parse "$COMMIT^{commit}")" = "$COMMIT" ] || { echo "template.sh: $SRC lacks $COMMIT" >&2; exit 1; }
WS="${DIR:?}/tokio"
# DIR/tokio is replaced only when this script made it (the marker sits beside
# it, so nothing extra lands in the workspace the sessions see).
MARK="$DIR/.tokio-template"
if [ -e "$WS" ] && [ ! -f "$MARK" ]; then
  echo "template.sh: $WS exists and was not made by this script; move it or pick another DIR" >&2
  exit 1
fi
rm -rf "${WS:?}"
mkdir -p "$WS"
echo "$COMMIT" >"$MARK"
git -C "$SRC" archive "$COMMIT" | tar -x -C "$WS"
git -C "$WS" init -q
git -C "$WS" add -A
git -C "$WS" -c user.email=eval@example.invalid -c user.name=eval commit -qm fixture
SCRATCH="$(mktemp -d "$DIR/home.XXXXXX")"
trap 'rm -rf "${SCRATCH:?}"' EXIT
if ! (cd "$WS" && HOME="$SCRATCH" CODE_GRAPH_NO_AUTO_UPDATE=1 "$BIN" incremental-index >"$SCRATCH/index.log" 2>&1); then
  cat "$SCRATCH/index.log" >&2
  echo "template.sh: indexing $WS with $BIN failed" >&2
  exit 1
fi
echo "template: $WS ($("$BIN" --version))"
