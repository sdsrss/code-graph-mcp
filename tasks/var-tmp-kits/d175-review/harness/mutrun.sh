#!/bin/bash
# Run each mutation against `cargo test --lib`; record failing tests.
OUT=/var/tmp/d175-review/mutations
mkdir -p $OUT
cd /var/tmp/d175-review/mut
for m in "$@"; do
  python3 -I /var/tmp/d175-review/harness/mutate.py apply $m || { echo "$m APPLY-FAILED" >> $OUT/summary.txt; continue; }
  CARGO_TARGET_DIR=/var/tmp/d175-review/target-mut cargo test --lib --no-fail-fast > $OUT/$m.log 2>&1
  rc=$?
  python3 -I /var/tmp/d175-review/harness/mutate.py restore $m
  fails=$(grep -E "^test .* FAILED$" $OUT/$m.log | sed 's/^test //; s/ \.\.\. FAILED$//' | tr '\n' ' ')
  res=$(grep -E "^test result" $OUT/$m.log | tail -1)
  if grep -q "error\[E" $OUT/$m.log; then res="COMPILE-ERROR"; fi
  echo "$m rc=$rc | $res | failed: $fails" >> $OUT/summary.txt
done
echo ALLDONE >> $OUT/summary.txt
