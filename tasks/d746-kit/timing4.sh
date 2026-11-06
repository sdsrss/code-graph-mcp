#!/bin/bash
# timing4.sh A B: fan-in tree, lib/m.ts gains `class K` with a method of each of its 50 names (every call meets two nodes), 3 runs per arm alternated, each from a fresh copy of that arm's index
W=/var/tmp/agent-tmp-d746; A="$1"; B="$2"; P=$W/perf
ms() { local s e; s=$(date +%s%N); (cd "$1" && "$W/bin/$2" incremental-index --no-embed --quiet >/dev/null 2>&1); e=$(date +%s%N); echo $(( (e-s)/1000000 )); }
K="export class K { $(for i in $(seq 0 49); do printf 'a%d() { return %d; } ' $i $i; done)}"
for arm in $A $B; do rm -rf "${P:?}/t4-$arm"; cp -a $P/fanin $P/t4-$arm; rm -rf "${P:?}/t4-$arm/.code-graph"; (cd $P/t4-$arm && $W/bin/$arm incremental-index --no-embed --quiet >/dev/null 2>&1); done
line="fanin gains methods"
for i in 1 2 3; do for arm in $A $B; do rm -rf "${P:?}/t4r-$arm"; cp -a $P/t4-$arm $P/t4r-$arm; echo "$K" >> $P/t4r-$arm/lib/m.ts; line="$line $arm=$(ms $P/t4r-$arm $arm)"; done; done
echo "$line (load $(cut -d' ' -f1 /proc/loadavg))"
for arm in $A $B; do echo "$arm after: calls=$(sqlite3 $P/t4r-$arm/.code-graph/index.db "SELECT COUNT(*) FROM edges WHERE relation='calls'") pending=$(sqlite3 $P/t4r-$arm/.code-graph/index.db "SELECT COUNT(*) FROM pending_unresolved_calls")"; done
for arm in $A $B; do rm -rf "${P:?}/t4f-$arm"; cp -a $P/t4r-$arm $P/t4f-$arm; rm -rf "${P:?}/t4f-$arm/.code-graph"; (cd $P/t4f-$arm && $W/bin/$arm incremental-index --no-embed --quiet >/dev/null 2>&1); echo "$arm rebuild: calls=$(sqlite3 $P/t4f-$arm/.code-graph/index.db "SELECT COUNT(*) FROM edges WHERE relation='calls'") pending=$(sqlite3 $P/t4f-$arm/.code-graph/index.db "SELECT COUNT(*) FROM pending_unresolved_calls")"; done
