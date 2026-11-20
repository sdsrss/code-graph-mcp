#!/usr/bin/env python3
"""pairdiff.py <armA> <armB> <gen.py> <first> <count> <steps> <work> <jobs>: per generated state, fresh index with
both arms; counts of edges only in A / only in B per relation (keys with lines, confidence, metadata) + samples."""
import importlib.util, json, os, random, shutil, sqlite3, subprocess, sys
from collections import Counter
from multiprocessing import Pool
R = "/var/tmp/agent-tmp-d909-review3"
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
Q = """SELECT e.relation, sf.path||':'||sn.name||'@'||sn.start_line, tf.path||':'||tn.name||'@'||tn.start_line,
 e.confidence, COALESCE(e.metadata,'') FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
A, B, GEN, FIRST, COUNT, STEPS, WORK, JOBS = sys.argv[1:9]
spec = importlib.util.spec_from_file_location("gen", GEN); gen = importlib.util.module_from_spec(spec); spec.loader.exec_module(gen)
def index(arm, files, d):
    assert d.startswith(R + "/")
    shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    for p, body in files.items():
        f = os.path.join(d, p); os.makedirs(os.path.dirname(f), exist_ok=True); open(f, "w").write(body)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([f"{R}/bin/{arm}", "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    if r.returncode: return None
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True); rows = Counter(con.execute(Q)); con.close(); return rows
def seed(s):
    rng = random.Random(s); st = gen.initial(rng); c = Counter(); smp = []
    d = os.path.join(WORK, f"p{s}")
    for i in range(int(STEPS) + 1):
        if i: st = gen.step(st, rng)
        a = index(A, st, d + "a"); b = index(B, st, d + "b")
        if a is None or b is None: c["error"] += 1; continue
        c["states"] += 1
        for k in (a - b).elements():
            c[("onlyA", k[0])] += 1
            if len(smp) < 3: smp.append(("onlyA", s, i, k))
        for k in (b - a).elements():
            c[("onlyB", k[0])] += 1
            if len(smp) < 3: smp.append(("onlyB", s, i, k))
    shutil.rmtree(d + "a", ignore_errors=True); shutil.rmtree(d + "b", ignore_errors=True)
    return c, smp
if __name__ == "__main__":
    os.makedirs(WORK, exist_ok=True); tot = Counter(); S = []
    with Pool(int(JOBS)) as p:
        for c, smp in p.imap_unordered(seed, range(int(FIRST), int(FIRST) + int(COUNT)), chunksize=4):
            tot.update(c); S += smp
    print(json.dumps({str(k): v for k, v in sorted(tot.items(), key=str)}))
    for x in S[:12]: print("SAMPLE", x)
