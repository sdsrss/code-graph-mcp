#!/usr/bin/env bash
# Score code-graph's Rust `calls` edges against rust-analyzer's SCIP index.
# Extra arguments go to oracle.py (e.g. --json-out FILE, --samples N).
#   CODE_GRAPH_BIN  code-graph binary to report the index state with (default: on PATH)
# The index is read, never written: a running MCP server keeps it fresh, and an
# indexing run here would race that server's writes. With no server running,
# run `code-graph-mcp incremental-index` first.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CG="${CODE_GRAPH_BIN:-code-graph-mcp}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/scip-oracle.XXXXXX")"
trap 'rm -rf "${WORK:?}"' EXIT

cd "$ROOT"
echo "code-graph:    $("$CG" --version)"
echo "rust-analyzer: $(rust-analyzer --version)"
echo "HEAD:          $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- '*.rs' || echo ' (+ uncommitted .rs changes)')"
echo "index:         $("$CG" health-check | head -1)"
rust-analyzer scip "$ROOT" --output "$WORK/index.scip" >"$WORK/ra.log" 2>&1 || { cat "$WORK/ra.log" >&2; exit 1; }
echo
python3 "$ROOT/scripts/scip_oracle/oracle.py" --scip "$WORK/index.scip" --root "$ROOT" "$@"
