"""parity.py <corpus> <edit> [arms]: full index, apply edit (from timing.EDITS), incremental, compare with fresh."""
import sys, os, collections
sys.path.insert(0, "/var/tmp/d175-review/harness")
from corp import *
import importlib.util
spec = importlib.util.spec_from_file_location("t", "/var/tmp/d175-review/harness/timing_edits.py"); t = importlib.util.module_from_spec(spec); spec.loader.exec_module(t)
corpus, edit = sys.argv[1], sys.argv[2]
arms = sys.argv[3].split(",") if len(sys.argv) > 3 else ["head", "base"]
BIN = {"head": f"{R}/bin/cgm-head", "base": f"{R}/bin/cgm-base", "b41": f"{R}/bin/cgm-b41"}
for a in arms:
    w = f"{R}/work/parity/{a}"
    inc, fr = w + "/inc", w + "/fresh"
    copy(corpus, inc); index(BIN[a], inc)
    for f in t.EDITS[edit]: f(inc)
    index(BIN[a], inc)
    copy(inc, fr); index(BIN[a], fr)
    oa, of = diff(dump(inc), dump(fr), 8, f"{edit} [{a}]")
