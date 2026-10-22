#!/usr/bin/env bash
# leak.sh <indexed-dir> <label> <sym...> -- <file...>: run user-facing commands with the head
# binary against the index as built (A) and with every synthetic import deleted (B); diff.
set -u
D=$1; L=$2; shift 2
SYMS=(); FILES=()
while [ $# -gt 0 ]; do [ "$1" = "--" ] && { shift; break; }; SYMS+=("$1"); shift; done
FILES=("$@")
R=/var/tmp/d520-review; OUT=$R/leak/$L; rm -rf "${OUT:?}"; mkdir -p $OUT
BIN=$R/bin/cgm-head
export HOME=$R/home CODE_GRAPH_NO_AUTO_UPDATE=1 CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_RESYNC_BUDGET=0
sqlite3 $D/.code-graph/index.db "PRAGMA wal_checkpoint(TRUNCATE);" >/dev/null
cp $D/.code-graph/index.db $OUT/A.db
cp $OUT/A.db $OUT/B.db
sqlite3 $OUT/B.db "UPDATE edges SET metadata = replace(metadata, '\"attr\":', '\"atr\":') WHERE relation='imports' AND metadata LIKE '%\"attr\":%';"
echo "synthetic in A: $(sqlite3 $OUT/A.db "select count(*) from edges where relation='imports' and metadata like '%\"attr\":%'") in B: $(sqlite3 $OUT/B.db "select count(*) from edges where relation='imports' and metadata like '%\"attr\":%'")"
cmds=("health-check --json" "report --json" "cycles --json" "map --json" "dead-code --json" "surprising --json" "surprising --include-tests --json" "centrality --json" "tour --json")
for s in "${SYMS[@]}"; do cmds+=("refs $s --json" "callgraph $s --json" "callgraph $s --direction callers --json" "impact $s --json" "show $s --json" "show $s --refs" "search $s --json" "ast-search $s --json"); done
for f in "${FILES[@]}"; do cmds+=("deps $f --json" "deps $f --direction incoming --json" "deps $f --direction both --json" "affected $f --json" "overview $f --json" "dead-code $f --json"); done
for arm in A B; do
  rm -f $D/.code-graph/index.db-wal $D/.code-graph/index.db-shm; cp $OUT/$arm.db $D/.code-graph/index.db
  i=0
  for c in "${cmds[@]}"; do
    i=$((i+1)); (cd $D && timeout 120 $BIN $c 2>&1 | sed -E 's/"(elapsed_ms|index_age|last_indexed_at|db_size_bytes|generated_at|took_ms|duration_ms)": ?[^,}]*//g; s/[0-9]+s ago//g; s/[0-9.]+ ?ms//g') > $OUT/$arm.$i.out
  done
  (cd $D && $BIN snapshot create --out $OUT/$arm.snap.db --quiet >/dev/null 2>&1; $BIN snapshot inspect $OUT/$arm.snap.db 2>&1 | sed -E 's/"(created_at|size_bytes|source_commit)": ?[^,}]*//g') > $OUT/$arm.snap.out
done
rm -f $D/.code-graph/index.db-wal $D/.code-graph/index.db-shm; cp $OUT/A.db $D/.code-graph/index.db
i=0; nd=0
for c in "${cmds[@]}"; do i=$((i+1)); if ! cmp -s $OUT/A.$i.out $OUT/B.$i.out; then nd=$((nd+1)); echo "DIFF: $c"; diff $OUT/A.$i.out $OUT/B.$i.out | head -8; fi; done
cmp -s $OUT/A.snap.out $OUT/B.snap.out || { echo "DIFF: snapshot inspect"; diff $OUT/A.snap.out $OUT/B.snap.out | head; }
echo "$L: ${#cmds[@]} commands, $nd differ"
