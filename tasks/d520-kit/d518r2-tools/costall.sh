#!/bin/bash
cd /var/tmp/d518r2
export ARMS=pre,base,head,headG
python3 tools/cost.py /var/tmp/d520/corpora/nx nx 15 networkx/algorithms/cycles.py networkx/__init__.py networkx/classes/graph.py NEW > logs/cost1.log 2>&1
python3 tools/cost.py /var/tmp/d520/corpora/std std 15 NEW json/__init__.py >> logs/cost1.log 2>&1
echo DONE >> logs/cost1.log
