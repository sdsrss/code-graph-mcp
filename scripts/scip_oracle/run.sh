#!/usr/bin/env bash
# Score code-graph's `calls` edges against a SCIP index of this repo.
#   run.sh [--language rust|javascript|python] [oracle.py args, e.g. --json-out FILE --samples N]
#   CODE_GRAPH_BIN  code-graph binary to report the index state with (default: on PATH)
#   SCIP_TOOLS_BIN  directory holding scip-typescript / scip-python (default: on PATH)
# The index is read, never written: a running MCP server keeps it fresh, and an
# indexing run here would race that server's writes. With no server running,
# run `code-graph-mcp incremental-index` first.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
CG="${CODE_GRAPH_BIN:-code-graph-mcp}"
LANGUAGE=rust
if [ "${1:-}" = "--language" ]; then LANGUAGE="${2:?--language needs a value}"; shift 2; fi
tool() { if [ -n "${SCIP_TOOLS_BIN:-}" ]; then echo "$SCIP_TOOLS_BIN/$1"; else echo "$1"; fi; }
WORK="$(mktemp -d "${TMPDIR:-/tmp}/scip-oracle.XXXXXX")"
trap 'rm -rf "${WORK:?}"' EXIT

# JS/Python indexers index a directory, not a file list: copy the working tree's
# files of that language (tracked + untracked, minus .gitignore'd, as the index
# sees them) so no tsconfig or build output is written into the repo, and score
# against the copy, whose lines are the ones the SCIP ranges point into.
copy_sources() {
  (cd "$ROOT" && git ls-files -co --exclude-standard -z -- "$@") |
    while IFS= read -r -d '' f; do
      mkdir -p "$WORK/src/$(dirname "$f")" && cp "$ROOT/$f" "$WORK/src/$f"
    done
}

cd "$ROOT"
echo "code-graph:    $("$CG" --version)"
echo "HEAD:          $(git rev-parse --short HEAD)$(git diff --quiet HEAD || echo ' (+ uncommitted changes)')"
echo "index:         $("$CG" health-check | head -1)"
SRC="$ROOT"
case "$LANGUAGE" in
  rust)
    echo "rust-analyzer: $(rust-analyzer --version)"
    rust-analyzer scip "$ROOT" --output "$WORK/index.scip" >"$WORK/scip.log" 2>&1 || { cat "$WORK/scip.log" >&2; exit 1; }
    ;;
  javascript)
    echo "scip-typescript: $("$(tool scip-typescript)" --version)"
    copy_sources '*.js' '*.mjs' '*.cjs' '*.jsx' '*.ts' '*.tsx' '*.mts' '*.cts'
    cat >"$WORK/src/tsconfig.json" <<'EOF'
{"compilerOptions": {"allowJs": true, "checkJs": false, "noEmit": true, "skipLibCheck": true,
  "target": "es2022", "module": "nodenext", "moduleResolution": "nodenext", "jsx": "preserve"},
 "include": ["**/*"]}
EOF
    (cd "$WORK/src" && "$(tool scip-typescript)" index --output "$WORK/index.scip") >"$WORK/scip.log" 2>&1 || { cat "$WORK/scip.log" >&2; exit 1; }
    SRC="$WORK/src"
    ;;
  python)
    echo "scip-python:   $("$(tool scip-python)" --version)"
    copy_sources '*.py'
    # --project-version: without a git checkout scip-python crashes on an undefined version.
    (cd "$WORK/src" && "$(tool scip-python)" index . --project-name "$(basename "$ROOT")" \
      --project-version 0 --output "$WORK/index.scip") >"$WORK/scip.log" 2>&1 || { cat "$WORK/scip.log" >&2; exit 1; }
    SRC="$WORK/src"
    ;;
  *) echo "run.sh: unknown --language $LANGUAGE (rust|javascript|python)" >&2; exit 2 ;;
esac
echo
python3 "$ROOT/scripts/scip_oracle/oracle.py" --language "$LANGUAGE" --scip "$WORK/index.scip" \
  --root "$SRC" --db "$ROOT/.code-graph/index.db" "$@"
