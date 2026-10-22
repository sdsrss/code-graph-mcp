#!/usr/bin/env bash
# scipgen.sh CORPUS LANG  -> /var/tmp/cg-rel164/index/oracle/CORPUS/{src,index.scip}
set -euo pipefail
ROOT=/var/tmp/cg-rel164/index/corp/pristine/$1; L=$2
W=/var/tmp/cg-rel164/index/oracle/$1; rm -rf "${W:?}"; mkdir -p "$W/src"
TB=/var/tmp/scip-tools/node_modules/.bin
copy_sources() { (cd "$ROOT" && git ls-files -co --exclude-standard -z -- "$@") | while IFS= read -r -d '' f; do mkdir -p "$W/src/$(dirname "$f")" && cp "$ROOT/$f" "$W/src/$f"; done; }
case $L in
 javascript)
  copy_sources '*.js' '*.mjs' '*.cjs' '*.jsx' '*.ts' '*.tsx' '*.mts' '*.cts'
  cat >"$W/src/tsconfig.json" <<'J'
{"compilerOptions": {"allowJs": true, "checkJs": false, "noEmit": true, "skipLibCheck": true,
  "target": "es2022", "module": "preserve", "jsx": "preserve"},
 "include": ["**/*"]}
J
  (cd "$W/src" && $TB/scip-typescript index --output "$W/index.scip") > "$W/scip.log" 2>&1 ;;
 python)
  copy_sources '*.py'
  (cd "$W/src" && $TB/scip-python index . --project-name "$1" --project-version 0 --output "$W/index.scip") > "$W/scip.log" 2>&1 ;;
 rust)
  rust-analyzer scip "$ROOT" --output "$W/index.scip" > "$W/scip.log" 2>&1 ;;
esac
ls -la "$W/index.scip"
