#!/usr/bin/env python3
"""subset_enum.py <base-bin> <head-bin> <gen.py> <first-seed> <count> <steps> <work> [jobs]

For every state of every generated sequence (the initial one and each step),
index the files from scratch with both binaries and report every edge the head
has that the base lacks (key: relation, both ends as path:name@line, confidence,
metadata). Prints totals and up to 20 head-only samples."""
import importlib.util, json, os, random, shutil, sqlite3, subprocess, sys
from multiprocessing import Pool
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
Q = """SELECT e.relation, sf.path||':'||sn.name||'@'||sn.start_line, tf.path||':'||tn.name||'@'||tn.start_line,
 e.confidence, COALESCE(e.metadata,'') FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
BASE, HEAD, GEN, FIRST, COUNT, STEPS, WORK = sys.argv[1:8]
JOBS = int(sys.argv[8]) if len(sys.argv) > 8 else 8
spec = importlib.util.spec_from_file_location("gen", GEN); gen = importlib.util.module_from_spec(spec); spec.loader.exec_module(gen)

def index(binary, files, d):
    shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    for p, body in files.items():
        f = os.path.join(d, p); os.makedirs(os.path.dirname(f), exist_ok=True); open(f, "w").write(body)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([binary, "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    if r.returncode:
        return None
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = set(con.execute(Q)); con.close()
    return rows

def seed(s):
    rng = random.Random(s)
    st = gen.initial(rng)
    out = {"seed": s, "states": 0, "head_only": [], "removed": 0, "errors": 0, "base_edges": 0}
    d = os.path.join(WORK, f"s{s}")
    for i in range(int(STEPS) + 1):
        if i:
            st = gen.step(st, rng)
        b = index(BASE, st, d + "b"); h = index(HEAD, st, d + "h")
        out["states"] += 1
        if b is None or h is None:
            out["errors"] += 1; continue
        out["base_edges"] += len(b)
        extra = h - b
        out["removed"] += len(b - h)
        if extra:
            out["head_only"].append({"step": i, "edges": sorted(map(list, extra)), "files": st})
    shutil.rmtree(d + "b", ignore_errors=True); shutil.rmtree(d + "h", ignore_errors=True)
    return out

if __name__ == "__main__":
    os.makedirs(WORK, exist_ok=True)
    seeds = range(int(FIRST), int(FIRST) + int(COUNT))
    tot = {"seeds": 0, "states": 0, "errors": 0, "removed": 0, "base_edges": 0, "head_only_states": 0, "head_only_edges": 0}
    samples = []
    with Pool(JOBS) as pool:
        for o in pool.imap_unordered(seed, seeds, chunksize=4):
            tot["seeds"] += 1; tot["states"] += o["states"]; tot["errors"] += o["errors"]
            tot["removed"] += o["removed"]; tot["base_edges"] += o["base_edges"]
            for h in o["head_only"]:
                tot["head_only_states"] += 1; tot["head_only_edges"] += len(h["edges"])
                if len(samples) < 20:
                    samples.append((o["seed"], h["step"], h["edges"][:4]))
    print(json.dumps(tot))
    for s in samples:
        print("HEAD-ONLY", s)
