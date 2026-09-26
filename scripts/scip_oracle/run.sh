#!/usr/bin/env bash
# Score code-graph's `calls` edges against a SCIP index of a repo (default: this one).
#   run.sh [--repo DIR] [--language rust|javascript|python|cpp] [oracle.py args, e.g. --json-out FILE --samples N]
#   CODE_GRAPH_BIN  code-graph binary to report the index state with (default: on PATH)
#   SCIP_TOOLS_BIN  directory holding scip-typescript / scip-python / scip-clang (default: on PATH)
#   COMPDB          compile_commands.json for --language cpp (default: DIR/compile_commands.json)
# The index is read, never written: a running MCP server keeps it fresh, and an
# indexing run here would race that server's writes. With no server running,
# run `code-graph-mcp incremental-index` first.
set -euo pipefail
ORACLE_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$ORACLE_DIR/../.." && pwd)"
CG="${CODE_GRAPH_BIN:-code-graph-mcp}"
LANGUAGE=rust
while [ $# -gt 0 ]; do
  case "$1" in
    --language) LANGUAGE="${2:?--language needs a value}"; shift 2 ;;
    --repo) ROOT="$(cd "${2:?--repo needs a directory}" && pwd)"; shift 2 ;;
    *) break ;;
  esac
done
tool() { if [ -x "${SCIP_TOOLS_BIN:-/nonexistent}/$1" ]; then echo "$SCIP_TOOLS_BIN/$1"; else echo "$1"; fi; }
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
    # module "preserve" resolves both `require()` and extensionless ESM imports
    # (`from './hono-base'`), which nodenext refuses and scores as external calls.
    cat >"$WORK/src/tsconfig.json" <<'EOF'
{"compilerOptions": {"allowJs": true, "checkJs": false, "noEmit": true, "skipLibCheck": true,
  "target": "es2022", "module": "preserve", "jsx": "preserve"},
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
  cpp)
    # scip-clang takes the compilation database, not a directory; paths in the
    # index are relative to the working directory, which must be the repo root.
    COMPDB="${COMPDB:-$ROOT/compile_commands.json}"
    [ -f "$COMPDB" ] || { echo "run.sh: --language cpp needs $COMPDB (cmake -DCMAKE_EXPORT_COMPILE_COMMANDS=ON, or set COMPDB)" >&2; exit 2; }
    echo "scip-clang:    $("$(tool scip-clang)" --version | head -1)"
    # --jobs=1: with parallel workers each header is indexed by whichever
    # translation unit reaches it first, and leveldb's gold moved between 3345
    # and 3394 pairs over identical input. One worker held it at 3394 over 3
    # runs (~28 s instead of ~4 s).
    (cd "$ROOT" && "$(tool scip-clang)" --compdb-path="$COMPDB" --index-output-path="$WORK/index.scip" --jobs=1) >"$WORK/scip.log" 2>&1 || { cat "$WORK/scip.log" >&2; exit 1; }
    ;;
  *) echo "run.sh: unknown --language $LANGUAGE (rust|javascript|python|cpp)" >&2; exit 2 ;;
esac
echo
python3 "$ORACLE_DIR/oracle.py" --language "$LANGUAGE" --scip "$WORK/index.scip" \
  --root "$SRC" --db "$ROOT/.code-graph/index.db" "$@"
