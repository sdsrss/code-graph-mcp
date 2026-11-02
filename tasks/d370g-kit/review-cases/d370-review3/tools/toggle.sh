#!/usr/bin/env bash
R=/var/tmp/d370-review3; TIMEFORMAT='user=%U wall=%R'
export HOME=$R/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 RUST_LOG=info
for i in 1 2 3 4; do
  for arm in base:6f4e17de head:001b74a9; do
    a=${arm%%:*}; b=${arm#*:}; C="$R/perf/std-$a"
    python3 -I - "$C/tkinter/__init__.py" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
add = "    def __init__(self, master, widgetName, cnf={}, kw={}, extra=()):\n        BaseWidget.__init__(self, master, widgetName, cnf, kw, extra)\n\n"
hdr = "class Widget(BaseWidget, Pack, Place, Grid):\n"
s = s.replace(hdr + add, hdr) if (hdr + add) in s else s.replace(hdr, hdr + add)
open(p, "w").write(s)
PY
    touch -d "+$((i*10)) seconds" "$C/tkinter/__init__.py"
    t=$( { time (cd "$C" && "$R/bin/$b" incremental-index --no-embed > /dev/null 2> "$R/out/toggle-$a-$i.err"); } 2>&1 )
    echo "$a toggle$i $t $(grep -o 're-extracting [0-9]* caller file(s): .*typed caller(s)' "$R/out/toggle-$a-$i.err")"
  done
done
