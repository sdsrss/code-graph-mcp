#!/bin/bash
# runall2.sh: the same suites, arms low7 (8f3f1670) and fix2 (03b01c4f, review repair).
set -u
L=/var/tmp/d521/logs2
for a in low7 fix2; do
  ln -sf /var/tmp/d521/bin/cgm-$a /var/tmp/d518r2/bin/cgm-$a
  ln -sf /var/tmp/d521/bin/cgm-$a /var/tmp/d520/bin/cgm-$a
done
p() { echo "$1 $(date +%T)" >> $L/progress; }
# 1. Full index, ten corpora.
true
cd /var/tmp/d518r2
( for c in flask hono express gin gson leveldb cgm; do python3 tools/fullcmp.py /var/tmp/d446/corpora/$c $c low7 fix2; done
  for c in nx std; do python3 tools/fullcmp.py /var/tmp/d520/corpora/$c $c low7 fix2; done
  python3 tools/fullcmp.py --plain /var/tmp/d521/corp/cgm-now cgmnow low7 fix2 ) > $L/full.log 2>&1
p full
for c in flask hono express gin gson leveldb cgm nx std cgmnow; do
  cmp -s full/$c/low7.edges full/$c/fix2.edges && echo "$c SAME" || echo "$c DIFF $(diff full/$c/low7.edges full/$c/fix2.edges | grep -c '^[<>]')"
done > $L/full-cmp.log
p fullcmp
# 2. The 994 scenarios of D#518's first review, plus round 2's twofile scenarios.
for s in twofile attr attr2 roots big big2 d370 fk review175; do
  ARMS=low7,fix2 J=6 python3 tools/par.py /var/tmp/d520-review/scn/$s.py > $L/rv-$s.log 2>&1; p "rv $s"
done
for s in twofile2 twofile3 twofile4; do
  ARMS=low7,fix2 J=6 python3 tools/par.py /var/tmp/d518r2/scn/$s.py > $L/r2-$s.log 2>&1; p "r2 $s"
done
# 3. 0.175.0's two reviews: 1,789 steps.
cd /var/tmp/d518r2/h175
export BINS="low7=/var/tmp/d521/bin/cgm-low7,fix2=/var/tmp/d521/bin/cgm-fix2"
(for s in /var/tmp/d520/harness/s1 /var/tmp/d520/harness/s2 /var/tmp/d520/harness/s3 /var/tmp/d520/harness/s4 /var/tmp/d520/harness/s5 /var/tmp/d520/harness/s6 /var/tmp/d520/harness/s7 /var/tmp/d520/harness/s8 /var/tmp/d520/scn/rename; do python3 scen.py $s.py 2>&1 | grep "^=="; done; python3 scen_cases.py /var/tmp/d175-r2/harness/cases{1,2,3,4,5,6}.py 2>&1 | grep "^==") > $L/scn175.log
p scn175
# 4. D#370(a)'s 65 parity scenarios + big_run_attr.
cd /var/tmp/d520/harness
for f in /var/tmp/d370a-review/scn/*.py /var/tmp/d370a-review2/scn/*.py /var/tmp/d520/scn518/big_run_attr.py; do
  python3 parity.py $f cgm-low7 cgm-fix2 2>&1 | grep "^\[cgm-"
done > $L/parity.log
p parity
# 5. flask random edits, seeds 1-3, 30 each.
for s in 1 2 3; do python3 fuzz.py /var/tmp/d446/corpora/flask $s 30 cgm-low7,cgm-fix2 > $L/fuzz-s$s.log 2>&1; p "fuzz $s"; done
p ALLDONE
