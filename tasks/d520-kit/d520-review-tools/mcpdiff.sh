#!/usr/bin/env bash
# mcpdiff.sh <indexed-dir> <label> <calls.json>: MCP tool answers on index A vs A minus synthetic imports.
D=$1; L=$2; C=$3; R=/var/tmp/d520-review; OUT=$R/leak/mcp-$L; rm -rf "${OUT:?}"; mkdir -p $OUT
sqlite3 $D/.code-graph/index.db "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null
cp $D/.code-graph/index.db $OUT/A.db; cp $OUT/A.db $OUT/B.db
sqlite3 $OUT/B.db "DELETE FROM edges WHERE relation='imports' AND metadata LIKE '%\"attr\":%';"
for arm in A B; do rm -f $D/.code-graph/index.db-wal $D/.code-graph/index.db-shm $D/.code-graph/usage.jsonl; cp $OUT/$arm.db $D/.code-graph/index.db
  python3 $R/tools/mcp.py $D $C | sed -E 's/\\"(elapsed_ms|index_age|took_ms|freshness)\\": ?[^,}]*//g; s/[0-9]+s ago//g' > $OUT/$arm.out; done
rm -f $D/.code-graph/index.db-wal $D/.code-graph/index.db-shm; cp $OUT/A.db $D/.code-graph/index.db
python3 - $OUT/A.out $OUT/B.out <<'PY'
import sys
def blocks(p):
    out=[];cur=None
    for l in open(p):
        if l.startswith("### "): cur=[l.strip(),""]; out.append(cur)
        elif cur: cur[1]+=l
    return out
a,b=blocks(sys.argv[1]),blocks(sys.argv[2]); n=0
for (ha,xa),(hb,xb) in zip(a,b):
    if xa!=xb: n+=1; print("DIFF", ha, "\n  A:", xa[:400], "\n  B:", xb[:400])
print(f"{len(a)} calls, {n} differ; errors in A: {sum('isError\": true' in x for _,x in a)}")
PY
