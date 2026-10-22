#!/usr/bin/env python3
"""parity.py <scenario.py> [bin ...]: scenario defines BEFORE (dict path->text) and STEPS
(list of dicts path->text|None; each dict is one incremental run). For each binary:
full index BEFORE, apply each step + incremental-index, then full-index the final tree
fresh and compare every edge (paths, names, lines, relation, metadata, confidence)."""
import os, shutil, subprocess, sys, sqlite3, time, json
scn = {}
exec(open(sys.argv[1]).read(), scn)
bins = sys.argv[2:] or ["head"]
B = "/var/tmp/d370a-review/bin"
name = os.path.basename(sys.argv[1]).rsplit('.', 1)[0]
work = "/var/tmp/d370a-review/par/" + name
home = "/var/tmp/d370a-review/fxhome"; os.makedirs(home, exist_ok=True)
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
if scn.get("LOG"): env["RUST_LOG"] = "info"
Q = """select coalesce(sf.path,'?'), sn.name, sn.start_line, coalesce(tf.path,'?'), tn.name, tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
QP = """select sf.path, sn.name, sn.start_line, p.target_name, coalesce(p.metadata,'') from pending_unresolved_calls p
        join nodes sn on sn.id=p.source_id join files sf on sf.id=sn.file_id"""
def write(d, files):
    for p, c in files.items():
        fp = os.path.join(d, p)
        if c is None:
            os.remove(fp); continue
        os.makedirs(os.path.dirname(fp), exist_ok=True)
        open(fp, "w").write(c)
        t = time.time() + 5
        os.utime(fp, (t, t))
def run(binary, d):
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--json"], cwd=d, env=env, capture_output=True, text=True)
    if r.returncode: print("FAIL", r.stderr[-3000:]); sys.exit(2)
    if scn.get("LOG"):
        for l in r.stderr.splitlines():
            if "fan-out" in l or "re-export" in l or "Phase 2b-final" in l: print("   LOG", l[-220:])
    return r.stdout
def dump(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    e = sorted(tuple(r) for r in con.execute(Q)); p = sorted(tuple(r) for r in con.execute(QP)); con.close()
    return e, p
def fmt(r): return f"{r[0]}:{r[1]}@{r[2]} -{r[6]}[{r[8]}]{r[7]}-> {r[3]}:{r[4]}@{r[5]}"
rc = 0
for b in bins:
    binary = os.path.join(B, b)
    inc = os.path.join(work, b, "inc"); ctl = os.path.join(work, b, "ctl")
    for d in (inc, ctl):
        if os.path.exists(d): shutil.rmtree(d)
        os.makedirs(d)
    tree = dict(scn["BEFORE"])
    write(inc, tree); subprocess.run(["git", "init", "-q"], cwd=inc, check=True)
    run(binary, inc)
    for i, step in enumerate(scn["STEPS"]):
        time.sleep(0.05)
        write(inc, step)
        for p, c in step.items():
            if c is None: tree.pop(p, None)
            else: tree[p] = c
        out = run(binary, inc)
        if scn.get("SHOWRUN"): print("   run", i, out.strip()[:300])
    write(ctl, tree); subprocess.run(["git", "init", "-q"], cwd=ctl, check=True)
    run(binary, ctl)
    (ei, pi), (ec, pc) = dump(inc), dump(ctl)
    from collections import Counter
    ci, cc = Counter(ei), Counter(ec)
    only_i = ci - cc; only_c = cc - ci
    py = lambda r: r[0].endswith('.py')
    eq = not only_i and not only_c
    print(f"[{b}] {name}: edges inc={len(ei)} rebuild={len(ec)} equal={eq} pending inc={len(pi)} rebuild={len(pc)} pending-equal={pi==pc}")
    for r in sorted(only_i): print("   INC-ONLY    ", fmt(r))
    for r in sorted(only_c): print("   REBUILD-ONLY", fmt(r))
    if pi != pc:
        for r in sorted(set(pi) - set(pc)): print("   PEND INC-ONLY", r)
        for r in sorted(set(pc) - set(pi)): print("   PEND REBUILD-ONLY", r)
    if scn.get("SHOW"):
        for r in ec:
            if any(s in fmt(r) for s in scn["SHOW"]): print("   rebuild:", fmt(r))
    if not eq: rc = 1
sys.exit(rc)
