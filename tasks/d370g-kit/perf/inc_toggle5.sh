#!/usr/bin/env bash
# inc_toggle5.sh: after run5.sh, toggle Widget.__init__ in each arm's tkinter/__init__.py 4 times; time each incremental run
W=/var/tmp/d370; TIMEFORMAT='user=%U wall=%R'
export HOME=/var/tmp/d370/parity/fxhome CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 RUST_LOG=info
for i in 1 2 3 4; do
  for arm in base-6f4e17de head-5; do
    C="$W/perf/std-$arm"
    python3 -I - "$C/tkinter/__init__.py" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
add = "    def __init__(self, master, widgetName, cnf={}, kw={}, extra=()):\n        BaseWidget.__init__(self, master, widgetName, cnf, kw, extra)\n\n"
hdr = "class Widget(BaseWidget, Pack, Place, Grid):\n"
s = s.replace(hdr + add, hdr) if (hdr + add) in s else s.replace(hdr, hdr + add)
open(p, "w").write(s)
PY
    touch -d "+$((i*10)) seconds" "$C/tkinter/__init__.py"
    t=$( { time (cd "$C" && "$W/bin/$arm" incremental-index --no-embed > /dev/null 2> "$W/perf/inc5-$arm-$i.err"); } 2>&1 )
    echo "$arm toggle$i $t $(grep -o 're-extracting [0-9]* caller file(s): .*typed caller(s)' "$W/perf/inc5-$arm-$i.err" | sed 's/bare-name caller(s) reached by a new same-name definition/bare/')"
  done
done
