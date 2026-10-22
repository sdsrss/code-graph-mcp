#!/usr/bin/env python3
"""scen.py <scenarios.py> [name ...]: each scenario is a list of steps; step 0 is
the initial tree, every later step maps path -> content (None deletes). For each
binary: index step 0 fresh, apply each later step + incremental-index, and after
EVERY step compare with a fresh index of the same cumulative tree."""
import collections, os, shutil, sqlite3, subprocess, sys, runpy
R = "/var/tmp/d175-review"
BINS = {"head": f"{R}/bin/cgm-head", "base": f"{R}/bin/cgm-base"}
if os.environ.get("BINS"):
    BINS = dict(x.split("=") for x in os.environ["BINS"].split(","))
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
Q = """select coalesce(sf.path,'?'), sn.name, coalesce(sn.qualified_name,''), sn.type,
              coalesce(tf.path,'?'), tn.name, coalesce(tn.qualified_name,''), tn.type,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def write(root, step):
    for p, c in step.items():
        fp = os.path.join(root, p)
        if c is None:
            if os.path.exists(fp): os.remove(fp)
        else:
            os.makedirs(os.path.dirname(fp), exist_ok=True)
            open(fp, "w").write(c)
def index(binary, root):
    r = subprocess.run([binary, "incremental-index", "--no-embed"], cwd=root, env=env, capture_output=True, text=True, timeout=600)
    if r.returncode: print("INDEX FAILED", root, r.stdout[-800:], r.stderr[-1500:])
    return r
def dump(root):
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    rows = collections.Counter(tuple(r) for r in con.execute(Q)); con.close(); return rows
def fmt(k):
    return f"{k[0]}:{k[2] or k[1]}({k[3]}) -{k[8]}[{k[10]}]-> {k[4]}:{k[6] or k[5]}({k[7]}) {k[9]}"
def fresh(binary, base, steps, upto):
    fr = f"{base}/fresh{upto}"
    os.makedirs(fr); subprocess.run(["git", "init", "-q", fr], check=True)
    for s in steps[:upto + 1]: write(fr, s)
    index(binary, fr); return dump(fr)
def run(name, steps):
    show = os.environ.get("SHOW")
    bad = 0
    for lab, b in BINS.items():
        w = f"{R}/work/{name}/{lab}"
        if os.path.exists(w): shutil.rmtree(w)
        inc = w + "/inc"
        os.makedirs(inc); subprocess.run(["git", "init", "-q", inc], check=True)
        for i, s in enumerate(steps):
            write(inc, s); index(b, inc)
            a = dump(inc)
            f = a if i == 0 else fresh(b, w, steps, i)
            oa, of = a - f, f - a
            print(f"== {name} [{lab}] step{i} inc={sum(a.values())} fresh={sum(f.values())} only-inc={sum(oa.values())} only-fresh={sum(of.values())}")
            for k in sorted(oa): print("   +inc  ", fmt(k))
            for k in sorted(of): print("   +fresh", fmt(k))
            bad += bool(oa or of)
            if show and (show == "all" or i == len(steps) - 1):
                for k in sorted(f): print("   fresh:", fmt(k))
    return bad
if __name__ == "__main__":
    scen = runpy.run_path(sys.argv[1])["SCENARIOS"]
    names = sys.argv[2:] or list(scen)
    for n in names: run(n, scen[n])
