#!/usr/bin/env bash
# Score code-graph's call edges on pinned open-source corpora, one per language
# this repo has little or none of: TypeScript, JavaScript, Python, C++.
#   corpora.sh DIR [oracle.py args]
# Clones into DIR (skipped when a clone exists), indexes each clone with
# code-graph (skipped when it has an index), and writes DIR/out/<corpus>.{txt,json}.
# Everything is written under DIR; delete it when done. Needs SCIP_TOOLS_BIN (or
# PATH) to hold scip-typescript, scip-python and scip-clang, and g++ for leveldb.
set -euo pipefail
DIR="$(mkdir -p "${1:?usage: corpora.sh DIR [oracle.py args]}" && cd "$1" && pwd)"
shift
ORACLE_DIR="$(cd "$(dirname "$0")" && pwd)"
CG="${CODE_GRAPH_BIN:-code-graph-mcp}"
# name  language  repo  tag  commit
CORPORA="
hono javascript honojs/hono v4.6.14 4eb101dd03e66e20dfda86e6e5ebce8045d61d05
express javascript expressjs/express 4.21.2 1faf228935aa0a13111f92c28ee795be64ce3f0f
flask python pallets/flask 3.1.0 ab8149664182b662453a563161aa89013c806dc9
leveldb cpp google/leveldb 1.23 99b3c03b3284f5886f9ef9a4ef703d57373e61be
"
mkdir -p "$DIR/out"
echo "$CORPORA" | while read -r name language repo tag commit; do
  [ -n "$name" ] || continue
  src="$DIR/$name"
  if [ ! -d "$src" ]; then
    git -c advice.detachedHead=false clone -q --depth 1 --branch "$tag" "https://github.com/$repo.git" "$src"
    [ "$(git -C "$src" rev-parse HEAD)" = "$commit" ] || { echo "corpora.sh: $repo@$tag is not $commit" >&2; exit 1; }
    if [ "$name" = leveldb ]; then
      # googletest/benchmark: compiled against, never indexed or scored
      git -C "$src" submodule update -q --init --depth 1
      echo "third_party/" >>"$src/.git/info/exclude"
    fi
  fi
  if [ "$language" = cpp ]; then
    # No cmake needed: leveldb builds with one flag set, and port_config.h is the
    # only generated header. Both live outside the clone.
    gen="$DIR/$name-gen"
    mkdir -p "$gen/port"
    printf '#define HAVE_FDATASYNC 1\n#define HAVE_FULLFSYNC 0\n#define HAVE_O_CLOEXEC 1\n#define HAVE_CRC32C 0\n#define HAVE_SNAPPY 0\n' >"$gen/port/port_config.h"
    python3 - "$src" "$gen" <<'EOF'
import json, subprocess, sys
root, gen = sys.argv[1], sys.argv[2]
files = [f for f in subprocess.check_output(["git", "-C", root, "ls-files", "*.cc"], text=True).split()
         if "windows" not in f]
args = ["g++", "-std=c++17", "-DLEVELDB_PLATFORM_POSIX=1", "-DLEVELDB_COMPILE_LIBRARY", "-I.", "-Iinclude",
        f"-I{gen}", "-Ithird_party/googletest/googletest/include",
        "-Ithird_party/googletest/googlemock/include", "-Ithird_party/benchmark/include"]
with open(f"{gen}/compile_commands.json", "w") as f:
    json.dump([{"directory": root, "file": p, "arguments": args + ["-c", p, "-o", "/dev/null"]} for p in files], f)
EOF
    export COMPDB="$gen/compile_commands.json"
  fi
  if [ ! -f "$src/.code-graph/index.db" ]; then
    (cd "$src" && "$CG" incremental-index >"$DIR/out/$name.index.log" 2>&1)
  fi
  "$ORACLE_DIR/run.sh" --repo "$src" --language "$language" --json-out "$DIR/out/$name.json" "$@" >"$DIR/out/$name.txt"
  echo "$name: $(sed -n 's/^  inferred *\([0-9]*\/[0-9]*\).*/\1/p' "$DIR/out/$name.txt" | tr '\n' ' ')(inferred precision, recall)"
done
