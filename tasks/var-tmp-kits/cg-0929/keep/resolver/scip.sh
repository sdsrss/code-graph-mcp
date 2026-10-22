#!/usr/bin/env bash
# Build each corpus's SCIP index ONCE (gold shared by every arm), mirroring run.sh.
#   scip.sh CORPUS LANG     -> /var/tmp/cg-0929/resolver/scip/CORPUS-LANG/{index.scip,src/}
set -euo pipefail
R=/var/tmp/cg-0929/resolver
C="$1"; L="$2"
ROOT="$R/corpora/$C"
OUT="$R/scip/$C-$L"
rm -rf "${OUT:?}"; mkdir -p "$OUT/src"
TOOLS=/var/tmp/scip-tools/node_modules/.bin
copy_sources() {
  (cd "$ROOT" && git ls-files -co --exclude-standard -z -- "$@") |
    while IFS= read -r -d '' f; do
      mkdir -p "$OUT/src/$(dirname "$f")" && cp "$ROOT/$f" "$OUT/src/$f"
    done
}
case "$L" in
  rust)
    rm -rf "${OUT:?}/src"; ln -s "$ROOT" "$OUT/src"
    rust-analyzer scip "$ROOT" --output "$OUT/index.scip" >"$OUT/scip.log" 2>&1 ;;
  javascript)
    copy_sources '*.js' '*.mjs' '*.cjs' '*.jsx' '*.ts' '*.tsx' '*.mts' '*.cts'
    cat >"$OUT/src/tsconfig.json" <<'EOF'
{"compilerOptions": {"allowJs": true, "checkJs": false, "noEmit": true, "skipLibCheck": true,
  "target": "es2022", "module": "preserve", "jsx": "preserve"},
 "include": ["**/*"]}
EOF
    (cd "$OUT/src" && "$TOOLS/scip-typescript" index --output "$OUT/index.scip") >"$OUT/scip.log" 2>&1 ;;
  python)
    copy_sources '*.py'
    (cd "$OUT/src" && "$TOOLS/scip-python" index . --project-name "$C" \
      --project-version 0 --output "$OUT/index.scip") >"$OUT/scip.log" 2>&1 ;;
esac
ls -la "$OUT/index.scip"
