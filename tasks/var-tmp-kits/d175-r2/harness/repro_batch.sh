#!/bin/bash
# usage: repro_batch.sh ARM  -> prints import edges of app_imp.py, incremental vs fresh
set -e
ARM=$1; B=/var/tmp/d175-r2/bin/$ARM; W=/var/tmp/d175-r2/work/repro_batch/$ARM
export CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1
rm -rf "${W:?}"; mkdir -p "$W/inc" "$W/fresh"
mk() { # dir mtag mbody
  mkdir -p "$1/pkg" "$1/f"; : > "$1/pkg/__init__.py"
  printf '%s' "$3" > "$1/pkg/m.py"
  printf '%sfrom pkg.m import Y\n\ndef imp_call():\n    return Y()\n' "$2" > "$1/app_imp.py"
  for i in $(seq -w 0 519); do printf '%sdef filler_%s():\n    return 1\n' "$2" "$i" > "$1/f/$i.py"; done
}
BOTH=$'def Y():\n    return 1\n\nclass C:\n    def Y(self):\n        return 2\n'
METH=$'class C:\n    def Y(self):\n        return 2\n'
(cd "$W/inc" && git init -q && mk . "" "$BOTH" && $B incremental-index --no-embed --quiet)
sleep 1
(cd "$W/inc" && mk . "# e"$'\n' "$METH" && $B incremental-index --no-embed --quiet)
(cd "$W/fresh" && git init -q && mk . "# e"$'\n' "$METH" && $B incremental-index --no-embed --quiet)
Q="SELECT e.relation||'['||e.confidence||'] '||COALESCE(e.metadata,'')||' -> '||ft.path||':'||COALESCE(nt.qualified_name,nt.name)||'('||nt.type||')' FROM edges e JOIN nodes ns ON ns.id=e.source_id JOIN files fs ON fs.id=ns.file_id JOIN nodes nt ON nt.id=e.target_id JOIN files ft ON ft.id=nt.file_id WHERE fs.path='app_imp.py' AND e.relation IN ('imports','calls') ORDER BY 1"
for d in inc fresh; do echo "--- $ARM $d"; sqlite3 "file:$W/$d/.code-graph/index.db?mode=ro" "$Q"; done
