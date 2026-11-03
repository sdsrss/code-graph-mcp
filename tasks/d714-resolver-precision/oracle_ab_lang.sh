#!/usr/bin/env bash
# One project, one language, BEFORE and AFTER binaries on the same corpus copy.
#   oracle_ab_lang.sh WORK NAME LANGUAGE SOURCE_REPO BEFORE_BIN AFTER_BIN
# Copies SOURCE_REPO's HEAD with `git archive` into WORK/corpora/NAME (once),
# indexes it with each binary (--no-embed), scores it, and keeps the judged
# dumps (WORK/out/NAME-LANG-ARM.judged.json) for classify_wrong_edges.py.
# Needs WORK/scip-tools for javascript/python. Delete WORK when done.
set -uo pipefail
W="${1:?work}"; P="${2:?name}"; L="${3:?language}"; SRC="${4:?source repo}"; BEFORE="${5:?before}"; AFTER="${6:?after}"
ORACLE=/home/ai/dev/code-graph-mcp/scripts/scip_oracle/run.sh
export SCIP_TOOLS_BIN="$W/scip-tools/node_modules/.bin" TMPDIR="$W/tmp"
export CODE_GRAPH_MODEL_DIR="$W/no-model" CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1
mkdir -p "$TMPDIR" "$CODE_GRAPH_MODEL_DIR" "$W/out" "$W/corpora"
d="$W/corpora/$P"
if [ ! -d "$d/.git" ]; then
  mkdir -p "$d" && git -C "$SRC" archive HEAD | tar -x -C "$d"
  (cd "$d" && git init -q . && git add -A && git -c user.name=a -c user.email=a@a commit -qm snap)
fi
for arm in before after; do
  bin=$BEFORE; [ "$arm" = after ] && bin=$AFTER
  rm -rf "${d:?}/.code-graph"
  (cd "$d" && "$bin" incremental-index --no-embed > "$W/out/$P-$L-$arm.index.log" 2>&1); echo "$P $L $arm index rc=$?" >> "$W/out/summary.txt"
  CODE_GRAPH_BIN="$bin" "$ORACLE" --repo "$d" --language "$L" --json-out "$W/out/$P-$L-$arm.json" \
    --dump-judged "$W/out/$P-$L-$arm.judged.json" --samples 40 > "$W/out/$P-$L-$arm.txt" 2>&1
  echo "$P $L $arm oracle rc=$?" >> "$W/out/summary.txt"
  cp "$d/.code-graph/index.db" "$W/out/$P-$L-$arm.index.db" 2>/dev/null
done
