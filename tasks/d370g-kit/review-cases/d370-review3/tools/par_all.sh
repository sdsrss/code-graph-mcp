#!/usr/bin/env bash
R=/var/tmp/d370-review3
for s in /var/tmp/d370/parity/scn{1,2,3,4,5}/*.py; do
  python3 -I $R/tools/parity.py "$s" 6f4e17de 001b74a9 2>&1 | grep -E "^\[|INC-ONLY|REBUILD-ONLY|FAIL" | sed "s|^|$(basename $(dirname $s))/|"
done
echo par-done
