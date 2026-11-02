#!/usr/bin/env bash
# run_all.sh: every scenario, both binaries; one summary line per (binary, scenario)
P=/var/tmp/d370/parity
for s in $P/scn1/*.py $P/scn2/*.py $P/scn3/*.py; do
  python3 -I $P/parity.py "$s" base head2 2>&1 | grep -E "^\[|INC-ONLY|REBUILD-ONLY|FAIL" | sed "s|^|$(basename $(dirname $s))/|"
done
