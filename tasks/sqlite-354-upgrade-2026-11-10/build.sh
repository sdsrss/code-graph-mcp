#!/bin/bash
# build.sh ARM -> release binary at $W/bin/ARM (default features)
W=/var/tmp/agent-tmp-sqlite-diff; arm=$1
export PATH=$HOME/.cargo/bin:$PATH CARGO_TARGET_DIR=$W/target-$arm
mkdir -p $W/bin
(cd $W/src-$arm && cargo build --release --locked) > $W/build-$arm.log 2>&1; rc=$?
cp $CARGO_TARGET_DIR/release/code-graph-mcp $W/bin/$arm 2>/dev/null
echo "$arm rc=$rc $(md5sum $W/bin/$arm 2>/dev/null | cut -c1-8) $($W/bin/$arm --version 2>/dev/null)"
