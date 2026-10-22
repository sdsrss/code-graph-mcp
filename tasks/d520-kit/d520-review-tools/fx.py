#!/usr/bin/env python3
"""fx.py <fixture.py>: FILES = {path: text}; index fresh with each arm, print calls edges
from non-<module> sources whose source file is in SHOW (or all), side by side."""
import os, sys, runpy, shutil, subprocess, sqlite3
R = "/var/tmp/d520-review"
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
fx = runpy.run_path(sys.argv[1]); files = fx["FILES"]; show = fx.get("SHOW")
Q = """select sf.path, sn.name, sn.start_line, tf.path, tn.name, tn.type, tn.start_line, e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
       where e.relation in ('calls') order by 1,3,2,4,7"""
res = {}
for arm in ("base", "head"):
    d = f"{R}/fx/{os.path.basename(sys.argv[1])[:-3]}/{arm}"
    if os.path.exists(d): shutil.rmtree(d)
    os.makedirs(d)
    for p, c in files.items():
        os.makedirs(os.path.dirname(os.path.join(d, p)) or d, exist_ok=True); open(os.path.join(d, p), "w").write(c)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
    if r.returncode: print(arm, "FAIL", r.stderr[-1000:])
    con = sqlite3.connect(f"{d}/.code-graph/index.db")
    res[arm] = set(con.execute(Q).fetchall())
def f(r): return f"{r[0]}:{r[1]}@{r[2]} -> {r[3]}:{r[4]}({r[5]}@{r[6]}) [{r[9]}] {r[8]}"
for r in sorted(res["base"] | res["head"]):
    if show and r[0] not in show: continue
    tag = "  both" if r in res["base"] and r in res["head"] else (" -BASE" if r in res["base"] else " +HEAD")
    print(tag, f(r))
