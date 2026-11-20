#!/bin/bash
W=/var/tmp/agent-tmp-d909-review3
cd $W/src/red || exit 1
export CARGO_TARGET_DIR=$W/tgt/red
for t in a_bare_call_binds_the_same_file_definition_its_scope_sees a_python_call_its_scopes_rule_out_binds_no_other_file d909_a_python_reference_binds_the_same_file_definition_its_scope_sees; do
  cargo test --release --locked --features embed-model --lib -j 4 -- --exact indexer::pipeline::tests::$t > $W/logs/red-$t.log 2>&1
  echo "$t rc=$?" >> $W/logs/red-summary.txt
done
# same three on f06 source
cd $W/src/f06f5e96 || exit 1
export CARGO_TARGET_DIR=$W/tgt/f06test
cargo test --release --locked --features embed-model --lib -j 4 > $W/logs/f06-libtests.log 2>&1
echo "f06 full lib rc=$?" >> $W/logs/red-summary.txt
