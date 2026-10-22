#!/usr/bin/env python3
"""pair.py <scenarios.py>...: for each scenario, run its incremental chain with arm A and
arm B (ARMS env, default head,headG) in separate dirs and compare the two incremental
indexes after every step (every edge with names/types/lines/metadata/confidence, and
pending rows). Prints one line per scenario with the steps where A and B differ."""
import collections, os, shutil, sqlite3, subprocess, sys, runpy, time
from concurrent.futures import ThreadPoolExecutor
R = "/var/tmp/d518r2"
A, B = os.environ.get("ARMS", "head,headG").split(",")
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
par = runpy.run_path(f"{R}/tools/par.py", run_name="lib")
def chain(name, steps, arm):
    w = f"{R}/pair/{name}/{arm}"
    if os.path.exists(w): shutil.rmtree(w)
    os.makedirs(w); subprocess.run(["git", "init", "-q", w], check=True)
    out = []
    for s in steps:
        par["write"](w, s)
        r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed"], cwd=w, env=env, capture_output=True, text=True, timeout=900)
        out.append(None if r.returncode else par["dump"](w))
    shutil.rmtree(f"{R}/pair/{name}", ignore_errors=True) if arm == B else None
    return out
def run(item):
    name, steps = item
    a = chain(name, steps, A); b = chain(name, steps, B)
    bad = [i for i, (x, y) in enumerate(zip(a, b)) if x != y]
    lines = [f"== {name}: {'SAME' if not bad else 'DIFFER@' + ','.join(map(str, bad))}"]
    for i in bad[:2]:
        if a[i] is None or b[i] is None: lines.append(f"   step{i} index failed"); continue
        (ea, pa), (eb, pb) = a[i], b[i]
        for k in sorted(ea - eb)[:10]: lines.append(f"   step{i} +{A} {par['fmt'](k)}")
        for k in sorted(eb - ea)[:10]: lines.append(f"   step{i} +{B} {par['fmt'](k)}")
        for k in sorted(pa - pb)[:5]: lines.append(f"   step{i} pend+{A} {k}")
        for k in sorted(pb - pa)[:5]: lines.append(f"   step{i} pend+{B} {k}")
    return "\n".join(lines)
if __name__ == "__main__":
    items = []
    for f in sys.argv[1:]:
        for n, s in runpy.run_path(f)["SCENARIOS"].items():
            items.append((os.path.basename(f)[:-3] + "_" + n, s))
    with ThreadPoolExecutor(int(os.environ.get("J", "6"))) as ex:
        for r in ex.map(run, items): print(r, flush=True)
