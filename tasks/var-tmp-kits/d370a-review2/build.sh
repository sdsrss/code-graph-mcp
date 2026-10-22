#!/bin/bash
set -u
R=/var/tmp/d370a-review2
for n in head base prev; do
  echo "=== build $n $(date +%T)"
  (cd "$R/src/$n" && CARGO_NET_OFFLINE=true CARGO_TARGET_DIR="$R/target-$n" cargo build --release --features embed-model > "$R/build-$n.log" 2>&1)
  rc=$?
  echo "rc=$rc $(date +%T)"
  if [ $rc -eq 0 ]; then
    cp "$R/target-$n/release/code-graph-mcp" "$R/bin/$n" && md5sum "$R/target-$n/release/code-graph-mcp" "$R/bin/$n"
  fi
done
