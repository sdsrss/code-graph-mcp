#!/usr/bin/env bash
R=/var/tmp/d370-review3
for v in 001b74a9 mut-own; do
  ( cd $R/src/$v && CARGO_TARGET_DIR=$R/target-test cargo test --lib test_a_call_decided_in_its_file_is_no_class_drift > $R/out/test-$v.log 2>&1; echo "exit $?" >> $R/out/test-$v.log )
done
echo test-done
