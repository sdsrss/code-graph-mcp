#!/usr/bin/env bash
# score.sh ARM BIN : copy corpora to arms/ARM, index with BIN (--no-embed), score with the SCIP oracle.
set -u
W=/var/tmp/d370; ARM="${1:?arm}"; BIN="${2:?bin}"
REPO=/home/ai/dev/code-graph-mcp
export TMPDIR=$W/tmp SCIP_TOOLS_BIN=$W/scip-tools/node_modules/.bin CODE_GRAPH_BIN="$BIN"
A="$W/arms/${ARM:?}"; mkdir -p "$A" "$W/out/$ARM"
for spec in ${CORPORA:-hono:javascript flask:python leveldb:cpp cobra:go gin:go std:python}; do
  name=${spec%%:*}; lang=${spec#*:}
  [ -d "$A/$name" ] || cp -a "$W/corpora/$name" "$A/$name"
  if [ ! -f "$A/$name/.code-graph/index.db" ]; then
    ( cd "$A/$name" && s=$SECONDS && "$BIN" incremental-index --no-embed; r=$?; echo "elapsed $((SECONDS-s)) s"; exit $r ) >"$W/out/$ARM/$name.index.log" 2>&1
    echo "$name index exit $?"
  fi
  [ "$lang" = go ] && continue
  if [ "$lang" = cpp ]; then
    gen="$A/$name-gen"; mkdir -p "$gen/port"
    printf '#define HAVE_FDATASYNC 1\n#define HAVE_FULLFSYNC 0\n#define HAVE_O_CLOEXEC 1\n#define HAVE_CRC32C 0\n#define HAVE_SNAPPY 0\n' >"$gen/port/port_config.h"
    python3 - "$A/$name" "$gen" <<'PY'
import json, subprocess, sys
root, gen = sys.argv[1], sys.argv[2]
files = [f for f in subprocess.check_output(["git", "-C", root, "ls-files", "*.cc"], text=True).split() if "windows" not in f]
args = ["g++", "-std=c++17", "-DLEVELDB_PLATFORM_POSIX=1", "-DLEVELDB_COMPILE_LIBRARY", "-I.", "-Iinclude", f"-I{gen}",
        "-Ithird_party/googletest/googletest/include", "-Ithird_party/googletest/googlemock/include", "-Ithird_party/benchmark/include"]
json.dump([{"directory": root, "file": p, "arguments": args + ["-c", p, "-o", "/dev/null"]} for p in files], open(f"{gen}/compile_commands.json", "w"))
PY
    export COMPDB="$gen/compile_commands.json"
  fi
  "$REPO/scripts/scip_oracle/run.sh" --repo "$A/$name" --language "$lang" --json-out "$W/out/$ARM/$name.json" \
     --dump-judged "$W/out/$ARM/$name.judged.json" --samples 0 >"$W/out/$ARM/$name.txt" 2>"$W/out/$ARM/$name.err"
  echo "$name score exit $?"
done
