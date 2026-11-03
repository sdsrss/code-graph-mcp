#!/usr/bin/env bash
# D#712 before/after on ONE corpus copy per project: index with the BEFORE binary,
# score, drop the index, index with the AFTER binary, score.
#   oracle_ab.sh WORK BEFORE_BIN AFTER_BIN
# Needs WORK/scip-tools (npm install --prefix WORK/scip-tools --save-exact
# @sourcegraph/scip-typescript@0.4.0). Delete WORK when done.
set -uo pipefail
W="${1:?work dir}"; BEFORE="${2:?before binary}"; AFTER="${3:?after binary}"
ORACLE=/home/ai/dev/code-graph-mcp/scripts/scip_oracle/run.sh
export SCIP_TOOLS_BIN="$W/scip-tools/node_modules/.bin" TMPDIR="$W/tmp"
export CODE_GRAPH_MODEL_DIR="$W/no-model" CODE_GRAPH_DISABLE_MODEL_DOWNLOAD=1
mkdir -p "$TMPDIR" "$CODE_GRAPH_MODEL_DIR" "$W/out" "$W/corpora"
snap() { # name source-repo
  local d="$W/corpora/$1"
  [ -d "$d/.git" ] && return
  mkdir -p "$d" && git -C "$2" archive HEAD | tar -x -C "$d"
  (cd "$d" && git init -q . && git add -A && git -c user.name=a -c user.email=a@a commit -qm snap)
}
pin() { # name repo tag commit
  local d="$W/corpora/$1"
  [ -d "$d/.git" ] && return
  git -c advice.detachedHead=false clone -q --depth 1 --branch "$3" "https://github.com/$2.git" "$d"
  [ "$(git -C "$d" rev-parse HEAD)" = "$4" ] || { echo "$2@$3 is not $4" >&2; exit 1; }
}
for p in daagu gsd-lite claudemd sgc claude-mem-lite; do snap "$p" "/home/ai/dev/$p"; done
pin hono honojs/hono v4.6.14 4eb101dd03e66e20dfda86e6e5ebce8045d61d05
pin express expressjs/express 4.21.2 1faf228935aa0a13111f92c28ee795be64ce3f0f
for p in daagu gsd-lite claudemd sgc claude-mem-lite hono express; do
  d="$W/corpora/$p"
  for arm in before after; do
    bin=$BEFORE; [ "$arm" = after ] && bin=$AFTER
    rm -rf "${d:?}/.code-graph"
    (cd "$d" && "$bin" incremental-index --no-embed > "$W/out/$p-$arm.index.log" 2>&1); echo "$p $arm index rc=$?" >> "$W/out/summary.txt"
    CODE_GRAPH_BIN="$bin" "$ORACLE" --repo "$d" --language javascript --json-out "$W/out/$p-$arm.json" --samples 40 > "$W/out/$p-$arm.txt" 2>&1
    echo "$p $arm oracle rc=$?" >> "$W/out/summary.txt"
  done
done
echo DONE >> "$W/out/summary.txt"
