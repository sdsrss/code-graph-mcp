#!/bin/bash
# Re-run the 994 scenarios of D#518's first review on v175, base, head.
cd /var/tmp/d518r2
for s in twofile attr attr2 roots big big2 d370 fk review175; do
  ARMS=v175,base,head J=6 python3 tools/par.py /var/tmp/d520-review/scn/$s.py > logs/rv-$s.log 2>&1
  echo "rv $s exit $? $(date +%T)" >> logs/runall.progress
done
echo ALL DONE >> logs/runall.progress
