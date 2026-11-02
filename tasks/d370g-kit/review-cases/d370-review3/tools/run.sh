#!/usr/bin/env bash
# run.sh CASEFILE.py : index the single file (copied as m.py, plus any sibling files in a dir) with base/a653/head and print call edges from m.py
set -u
R=/var/tmp/d370-review3
src="${1:?case}"; name=$(basename "$src" .py)
for b in 6f4e17de a653b1a1 001b74a9; do
  d="$R/w/$name/$b"; rm -rf "${d:?}"; mkdir -p "$d"
  if [ -d "$src" ]; then cp -r "$src"/. "$d"/; else cp "$src" "$d/m.py"; fi
  ( cd "$d" && git init -q && HOME=$R/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 "$R/bin/$b" incremental-index --no-embed >/dev/null 2>"$d.err" ) || { echo "$b index FAIL"; tail -n 5 "$d.err"; }
  echo "== $b"
  python3 -I "$R/tools/edges.py" "$d/.code-graph/index.db" ${FILTER:-}
done
