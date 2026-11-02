#!/usr/bin/env bash
# toggle Widget.__init__ in tkinter/__init__.py and time each incremental run, alternating binaries
export HOME=/var/tmp/d370-review/home CODE_GRAPH_NO_UPDATE_CHECK=1 CODE_GRAPH_NO_AUTO_UPDATE=1 RUST_LOG=info
TIMEFORMAT='user=%U wall=%R'
C=/var/tmp/d370-review/corp
for i in 1 2 3 4; do
  for b in base head; do
    python3 -I - "$C/std-$b/tkinter/__init__.py" <<'PY'
import sys
p = sys.argv[1]; s = open(p).read()
add = "    def __init__(self, master, widgetName, cnf={}, kw={}, extra=()):\n        BaseWidget.__init__(self, master, widgetName, cnf, kw, extra)\n\n"
hdr = "class Widget(BaseWidget, Pack, Place, Grid):\n"
s = s.replace(hdr + add, hdr) if (hdr + add) in s else s.replace(hdr, hdr + add)
open(p, "w").write(s)
PY
    touch -d "+$((i*10)) seconds" "$C/std-$b/tkinter/__init__.py"
    cd "$C/std-$b" || exit 1
    t=$( { time /var/tmp/d370-review/parity/bin/$b incremental-index --no-embed > /dev/null 2> "$C/inc-$b-$i.err" ; } 2>&1 )
    echo "$b toggle$i $t $(grep -o 're-extracting [0-9]* caller file(s): .*typed caller(s)' "$C/inc-$b-$i.err" | sed 's/bare-name caller(s) reached by a new same-name definition/bare/')"
  done
done
