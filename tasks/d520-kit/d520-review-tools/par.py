#!/usr/bin/env python3
"""par.py <scenarios.py> [name-substring ...]: SCENARIOS = {name: [step0, step1, ...]};
a step maps path -> text (None deletes). For each arm (ARMS env, default base,head):
index step0 fresh, then each later step incrementally; after EVERY step compare the
incremental index (edges with qualified names/types/lines/metadata/confidence, and
pending rows) with a fresh index of the same cumulative tree. Exit summary per arm."""
import collections, os, shutil, sqlite3, subprocess, sys, runpy, time, json
from concurrent.futures import ThreadPoolExecutor
R = "/var/tmp/d520-review"
ARMS = os.environ.get("ARMS", "base,head").split(",")
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
Q = """select coalesce(sf.path,'?'), sn.name, coalesce(sn.qualified_name,''), sn.type, sn.start_line,
              coalesce(tf.path,'?'), tn.name, coalesce(tn.qualified_name,''), tn.type, tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
QP = """select sf.path, sn.name, sn.start_line, p.target_name, coalesce(p.metadata,'') from pending_unresolved_calls p
        join nodes sn on sn.id=p.source_id join files sf on sf.id=sn.file_id"""
def write(root, step):
    t = time.time() + 2
    for p, c in step.items():
        fp = os.path.join(root, p)
        if c is None:
            if os.path.exists(fp): os.remove(fp)
        else:
            os.makedirs(os.path.dirname(fp) or root, exist_ok=True)
            open(fp, "w").write(c); os.utime(fp, (t, t))
def index(arm, root):
    r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed"], cwd=root, env=env, capture_output=True, text=True, timeout=900)
    if r.returncode: print("INDEX FAILED", arm, root, r.stdout[-800:], r.stderr[-1500:], flush=True)
    return r.returncode
def dump(root):
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    e = collections.Counter(tuple(r) for r in con.execute(Q))
    p = collections.Counter(tuple(r) for r in con.execute(QP)); con.close(); return e, p
def fmt(k):
    return f"{k[0]}:{k[2] or k[1]}({k[3]}@{k[4]}) -{k[10]}[{k[12]}]-> {k[5]}:{k[7] or k[6]}({k[8]}@{k[9]}) {k[11]}"
def run_arm(name, steps, arm):
    out = []; bad = []
    w = f"{R}/par/{name}/{arm}"
    if os.path.exists(w): shutil.rmtree(w)
    inc = w + "/inc"; os.makedirs(inc); subprocess.run(["git", "init", "-q", inc], check=True)
    tree = {}
    for i, s in enumerate(steps):
        write(inc, s)
        for p, c in s.items():
            if c is None: tree.pop(p, None)
            else: tree[p] = c
        if index(arm, inc): bad.append((i, "FAIL")); continue
        if i == 0: continue
        fr = f"{w}/fresh{i}"; os.makedirs(fr); subprocess.run(["git", "init", "-q", fr], check=True)
        write(fr, tree); index(arm, fr)
        (a, ap), (f, fp) = dump(inc), dump(fr)
        oa, of = a - f, f - a; pa, pf = ap - fp, fp - ap
        if oa or of or pa or pf:
            bad.append((i, f"+inc={sum(oa.values())} +fresh={sum(of.values())} pend+inc={sum(pa.values())} pend+fresh={sum(pf.values())}"))
            if not os.environ.get("QUIET"):
                for k in sorted(oa): out.append(f"   [{arm}] step{i} +inc   {fmt(k)}")
                for k in sorted(of): out.append(f"   [{arm}] step{i} +fresh {fmt(k)}")
                for k in sorted(pa): out.append(f"   [{arm}] step{i} pend+inc   {k}")
                for k in sorted(pf): out.append(f"   [{arm}] step{i} pend+fresh {k}")
        if os.environ.get("SHOW") and i == len(steps) - 1:
            for k in sorted(f):
                if any(s in fmt(k) for s in os.environ["SHOW"].split(",")): out.append(f"   [{arm}] fresh: {fmt(k)}")
        shutil.rmtree(fr)
    return arm, bad, out
def run(name, steps):
    res = [run_arm(name, steps, a) for a in ARMS]
    lines = [f"== {name}: " + "  ".join(f"{a}:{'OK' if not b else 'DIFF@' + ','.join(str(x[0]) for x in b)}" for a, b, _ in res)]
    for a, b, o in res:
        for i, d in b: lines.append(f"   [{a}] step{i} {d}")
        lines.extend(o)
    return "\n".join(lines)
if __name__ == "__main__":
    scen = runpy.run_path(sys.argv[1])["SCENARIOS"]
    pick = sys.argv[2:]
    items = [(n, s) for n, s in scen.items() if not pick or any(p in n for p in pick)]
    with ThreadPoolExecutor(int(os.environ.get("J", "4"))) as ex:
        for r in ex.map(lambda x: run(*x), items): print(r, flush=True)
