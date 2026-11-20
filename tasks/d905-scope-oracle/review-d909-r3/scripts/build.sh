#!/bin/bash
W=/var/tmp/agent-tmp-d909-review3
for a in f06f5e96 c52b115c v0.203.0; do
  cd "$W/src/$a" || exit 1
  start=$(date +%s)
  CARGO_TARGET_DIR="$W/tgt/$a" cargo build --release --locked --features embed-model -j 8 > "$W/logs/build-$a.log" 2>&1
  rc=$?
  echo "$a rc=$rc secs=$(( $(date +%s) - start ))" >> "$W/logs/build-summary.txt"
  [ $rc -eq 0 ] && cp "$W/tgt/$a/release/code-graph-mcp" "$W/bin/$a" && md5sum "$W/bin/$a" >> "$W/logs/build-summary.txt"
done
