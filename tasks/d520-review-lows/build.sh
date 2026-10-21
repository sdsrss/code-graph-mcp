#!/bin/bash
# build.sh <sha> <arm>: release build (embed-model, as shipped) of a committed tree,
# own source copy, shared target under /var/tmp/d521; binary copied to bin/cgm-<arm>.
set -u
S="$1"; A="$2"; R=/var/tmp/d521
export PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$R/target
rm -rf "${R:?}/src-${A:?}" && mkdir -p "$R/src-$A"
git -C /home/ai/dev/code-graph-mcp archive "$S" | tar -x -C "$R/src-$A"
# git archive stamps the commit time; touch so cargo cannot reuse the previous arm (shared target).
find "$R/src-$A" -type f -exec touch {} +
( cd "$R/src-$A" && cargo build --release --features embed-model ) > "$R/logs/build-$A.log" 2>&1
rc=$?
echo "exit $rc" >> "$R/logs/build-$A.log"
[ "$rc" = 0 ] && cp "$R/target/release/code-graph-mcp" "$R/bin/cgm-$A" && md5sum "$R/bin/cgm-$A" >> "$R/logs/build-$A.log"
exit $rc
