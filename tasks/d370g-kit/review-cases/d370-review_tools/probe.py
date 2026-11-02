#!/usr/bin/env python3
"""probe.py CASE.py [bin ...]: CASE defines FILES (dict path->text). Full-index a
fresh copy with each binary (bins from /var/tmp/d370-review/parity/bin) and print
call edges (source file:qname@line -> target file:qname, meta, confidence),
then the edges that differ between the first binary and each other one."""
import os, shutil, subprocess, sys, sqlite3, json
scn = {}
exec(open(sys.argv[1]).read(), scn)
bins = sys.argv[2:] or ["base", "head2"]
B = "/var/tmp/d370-review/parity/bin"
name = os.path.basename(sys.argv[1]).rsplit('.', 1)[0]
work = "/var/tmp/d370-review/runs/" + name
home = "/var/tmp/d370-review/home"
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
Q = """select coalesce(sf.path,'?'), coalesce(sn.qualified_name, sn.name), sn.start_line,
              coalesce(tf.path,'?'), coalesce(tn.qualified_name, tn.name), tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id
       where e.relation='calls'"""
QP = """select sf.path, coalesce(sn.qualified_name, sn.name), p.target_name, coalesce(p.metadata,'') from pending_unresolved_calls p
        join nodes sn on sn.id=p.source_id join files sf on sf.id=sn.file_id"""
res = {}
for b in bins:
    d = os.path.join(work, b)
    if os.path.exists(d): shutil.rmtree(d)
    os.makedirs(d)
    for p, c in scn["FILES"].items():
        fp = os.path.join(d, p)
        os.makedirs(os.path.dirname(fp), exist_ok=True)
        open(fp, "w").write(c)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([os.path.join(B, b), "incremental-index", "--no-embed", "--json"], cwd=d, env=env, capture_output=True, text=True)
    if r.returncode:
        print("FAIL", b, r.stderr[-3000:]); sys.exit(2)
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    e = sorted(tuple(x) for x in con.execute(Q))
    p = sorted(tuple(x) for x in con.execute(QP))
    con.close()
    res[b] = (e, p)
def fmt(r): return f"{r[0]}:{r[1]}@{r[2]} -> {r[3]}:{r[4]}@{r[5]} [{r[8]}] {r[7]}"
FILT = scn.get("FILTER")
for b in bins:
    e, p = res[b]
    print(f"== {b}: calls={len(e)} pending={len(p)}")
    if scn.get("QUIET"): continue
    for r in e:
        if FILT and not any(s in fmt(r) for s in FILT): continue
        print("   ", fmt(r))
    for r in p:
        print("    PENDING", r)
b0 = bins[0]
for b in bins[1:]:
    a = set(res[b0][0]); c = set(res[b][0])
    print(f"== diff {b0} -> {b}: lost={len(a-c)} added={len(c-a)}")
    for r in sorted(a - c): print("   LOST ", fmt(r))
    for r in sorted(c - a): print("   ADDED", fmt(r))
