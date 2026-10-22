#!/bin/bash
cd /var/tmp/d518r2
for c in flask hono express gin gson leveldb cgm; do python3 tools/fullcmp.py /var/tmp/d446/corpora/$c $c pre base head; done > logs/full.log 2>&1
for c in nx std; do python3 tools/fullcmp.py /var/tmp/d520/corpora/$c $c pre base head; done >> logs/full.log 2>&1
python3 tools/fullcmp.py --plain /var/tmp/d518r2/corp/cgm-now cgmnow pre base head >> logs/full.log 2>&1
echo DONE >> logs/full.log
