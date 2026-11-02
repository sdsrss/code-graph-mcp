#!/usr/bin/env python3
"""shapes.py SHAPES.py BIN: SHAPES defines CASES = [(label, code)], each code a
whole file whose `m` calls are checked. One project, one file per case; prints
the q/v metadata of every call edge or pending row named `m` per file."""
import os, shutil, subprocess, sys, sqlite3
scn = {}
exec(open(sys.argv[1]).read(), scn)
b = sys.argv[2]
name = os.path.basename(sys.argv[1]).rsplit('.', 1)[0]
d = f"/var/tmp/d370-review/runs/{name}/{b}"
if os.path.exists(d): shutil.rmtree(d)
os.makedirs(d)
env = dict(os.environ, HOME="/var/tmp/d370-review/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
for i, (label, code) in enumerate(scn["CASES"]):
    open(os.path.join(d, f"s{i:02d}.py"), "w").write(code)
for p, c in scn.get("EXTRA", {}).items():
    open(os.path.join(d, p), "w").write(c)
subprocess.run(["git", "init", "-q"], cwd=d, check=True)
r = subprocess.run([f"/var/tmp/d370-review/parity/bin/{b}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
if r.returncode: print(r.stderr[-2000:]); sys.exit(2)
con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
tgt = scn.get("TARGET", "m")
for i, (label, code) in enumerate(scn["CASES"]):
    f = f"s{i:02d}.py"
    rows = con.execute("""select distinct coalesce(e.metadata,''), tf.path||':'||coalesce(t.qualified_name,t.name) from edges e join nodes s on s.id=e.source_id
        join files sf on sf.id=s.file_id join nodes t on t.id=e.target_id join files tf on tf.id=t.file_id
        where e.relation='calls' and sf.path=? and t.name=?""", (f, tgt)).fetchall()
    pend = con.execute("""select distinct coalesce(p.metadata,'') from pending_unresolved_calls p join nodes s on s.id=p.source_id
        join files sf on sf.id=s.file_id where sf.path=? and p.target_name=?""", (f, tgt)).fetchall()
    qs = sorted({m for m, _ in rows} | {"PENDING " + m for (m,) in pend})
    print(f"{f} {label:40s} {qs}  {sorted({t for _, t in rows})}")
