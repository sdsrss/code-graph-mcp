#!/usr/bin/env python3
"""parity.py <binary> <workdir> <scenario.json>
scenario: {"v1": {path: content}, "steps": [[{path: content|null}, ...]]}
Runs: full index of v1, then each step applied + incremental-index (arm INC).
Then copies final tree, wipes .code-graph, full index (arm FULL). Diffs edges."""
import json, os, shutil, sqlite3, subprocess, sys
binary, work, scen = sys.argv[1], sys.argv[2], json.load(open(sys.argv[3]))
env = dict(os.environ, HOME="/var/tmp/backlog-review/home", CODE_GRAPH_NO_UPDATE_CHECK="1")
def run(root, *args):
    r = subprocess.run([binary, *args], cwd=root, env=env, capture_output=True, text=True, timeout=600)
    return r
def write(root, files):
    for p, c in files.items():
        full = os.path.join(root, p)
        if c is None:
            if os.path.exists(full): os.remove(full)
            continue
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "w") as f: f.write(c)
def dump(root):
    con = sqlite3.connect(os.path.join(root, ".code-graph/index.db"))
    q = """select coalesce(sf.path,'?'), sn.name, coalesce(sn.qualified_name,''), sn.type,
                  coalesce(tf.path,'?'), tn.name, coalesce(tn.qualified_name,''), tn.type,
                  e.relation, e.confidence
           from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                        join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
    rows = sorted(tuple(r) for r in con.execute(q))
    nodes = sorted(tuple(r) for r in con.execute("select coalesce(f.path,'?'), n.name, n.type from nodes n left join files f on f.id=n.file_id"))
    con.close()
    return rows, nodes
inc = os.path.join(work, "inc"); full = os.path.join(work, "full")
for d in (inc, full):
    if os.path.exists(d): shutil.rmtree(d)
os.makedirs(inc)
subprocess.run(["git", "init", "-q", inc])
write(inc, scen["v1"])
r = run(inc, "incremental-index")
if r.returncode: print("v1 index failed", r.stderr[-2000:]); sys.exit(2)
v1rows, _ = dump(inc)
for i, step in enumerate(scen["steps"]):
    write(inc, step)
    r = run(inc, "incremental-index")
    if r.returncode: print("step", i, "failed", r.stderr[-2000:]); sys.exit(2)
shutil.copytree(inc, full, ignore=shutil.ignore_patterns(".code-graph"))
r = run(full, "incremental-index")
if r.returncode: print("full failed", r.stderr[-2000:]); sys.exit(2)
a, an = dump(inc); b, bn = dump(full)
sa, sb = set(a), set(b)
interesting = scen.get("show", None)
def keep(r): return interesting is None or any(s in "|".join(r) for s in interesting)
print(f"INC edges={len(a)} FULL edges={len(b)} only-INC={len(sa-sb)} only-FULL={len(sb-sa)} nodes-diff={len(set(an)^set(bn))}")
for r in sorted(sa - sb): print("  only INC :", " | ".join(r))
for r in sorted(sb - sa): print("  only FULL:", " | ".join(r))
if scen.get("print_final"):
    for r in b:
        if keep(r): print("  FULL:", " | ".join(r))
