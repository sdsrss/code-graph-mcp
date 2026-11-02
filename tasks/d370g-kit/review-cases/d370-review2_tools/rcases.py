import os, shutil, subprocess, sqlite3, sys, collections
R = "/var/tmp/d370-review2/rcases"
env = dict(os.environ, HOME="/var/tmp/d370-review2/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
Q = """select sf.path, sn.name, sn.start_line, e.relation, tf.path, coalesce(tn.qualified_name, tn.name), tn.start_line, e.confidence
from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
for f in sorted(os.listdir("/var/tmp/d370-review/cases")):
    scn = {}
    exec(open(os.path.join("/var/tmp/d370-review/cases", f)).read(), scn)
    if "FILES" not in scn: print(f, "no FILES"); continue
    out = {}
    for b in ("base-6f4e17de", "head-4"):
        d = os.path.join(R, f[:-3], b)
        shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
        for p, c in scn["FILES"].items():
            os.makedirs(os.path.dirname(os.path.join(d, p)), exist_ok=True); open(os.path.join(d, p), "w").write(c)
        subprocess.run(["git", "init", "-q"], cwd=d, check=True)
        subprocess.run(["/var/tmp/d370/bin/" + b, "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, check=True)
        con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
        out[b] = collections.Counter(con.execute(Q))
    bs, hs = out["base-6f4e17de"], out["head-4"]
    rel = "equal" if bs == hs else ("head strict subset" if not (hs - bs) else ("head strict superset" if not (bs - hs) else "differ both ways"))
    print(f"{f}: {rel}  base={sum(bs.values())} head={sum(hs.values())}")
    for r in (hs - bs): print("   HEAD-ONLY", r)
    for r in (bs - hs): print("   BASE-ONLY", r)
