#!/bin/bash
W=/var/tmp/agent-tmp-d909-review3
cd $W/work || exit 1
T=$W/scripts/timeab3.sh
$T $W/work/tm 3 $W/tin/std $W/tin/daagu $W/tin/ann480_adv2 $W/tin/ann480_k5 $W/tin/c14_defs100_ann20 $W/tin/args120 \
  $W/tin/k1b_classes_deep30 $W/tin/k1bd_defs_deep30 $W/tin/k1c_oneline_classes_deep30 $W/tin/k2b_class_per_level30 \
  $W/cost/a1_nested_calls_16k $W/cost/a2_header_disagree_16k $W/cost/a4_header_towers_150x30 $W/cost/a7_js_nested_calls_16k > $W/logs/timing-main.txt 2>&1
ARMS="c52b115c f06f5e96" $T $W/work/tm 3 $W/cost/a3_nested_class_calls_4k >> $W/logs/timing-main.txt 2>&1
echo TDONE >> $W/logs/timing-main.txt
$W/scripts/timeab3.sh $W/work/tm 2 $W/cost/a5_min_nested_calls_40k > $W/logs/timing-a5.txt 2>&1
$W/scripts/timeab3.sh $W/work/tm 1 $W/cost/a6_min_header_disagree_37k >> $W/logs/timing-a5.txt 2>&1
echo TDONE >> $W/logs/timing-a5.txt
