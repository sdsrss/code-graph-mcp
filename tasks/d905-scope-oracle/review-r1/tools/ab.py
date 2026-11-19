#!/usr/bin/env python3
"""ab.py <case-dir> [--show all|diff] : index a copy with base and head, print edges diff (python calls/references)."""
import os, shutil, sqlite3, subprocess, sys
R = "/var/tmp/d905-review"
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
ROWS = """SELECT e.relation, sf.path, COALESCE(sn.qualified_name, sn.name), sn.type, tf.path, COALESCE(tn.qualified_name, tn.name), tn.type, e.confidence, COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
def index(binary, src, dest):
    if os.path.exists(dest): shutil.rmtree(dest)
    shutil.copytree(src, dest)
    subprocess.run("git init -q && git add -A && git -c user.email=a@b -c user.name=r commit -qm x", shell=True, cwd=dest, check=True)
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=dest, env=ENV, capture_output=True, text=True)
    if r.returncode: sys.exit(r.stderr)
    con = sqlite3.connect(f"file:{dest}/.code-graph/index.db?mode=ro", uri=True)
    rows = sorted(con.execute(ROWS)); con.close(); return rows
case = os.path.abspath(sys.argv[1]); show = sys.argv[2] if len(sys.argv) > 2 else "diff"
name = os.path.basename(case)
b = index(f"{R}/base-bin", case, f"{R}/work/{name}-base")
h = index(f"{R}/head-bin", case, f"{R}/work/{name}-head")
from collections import Counter
cb, ch = Counter(b), Counter(h)
fmt = lambda r: f"{r[0]:10} {r[1]}:{r[2]}({r[3]}) -> {r[4]}:{r[5]}({r[6]}) {r[7]} {r[8]}"
if show == "all":
    for r in h: print("  ", fmt(r))
for r in sorted((cb - ch).elements()): print("BASE-ONLY", fmt(r))
for r in sorted((ch - cb).elements()): print("HEAD-ONLY", fmt(r))
print(f"edges base={len(b)} head={len(h)}")
