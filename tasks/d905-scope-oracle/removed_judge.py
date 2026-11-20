#!/usr/bin/env python3
"""removed_judge.py <base-bin> <head-bin> <gen.py> <first-seed> <count> <steps> <work> [jobs]

Like subset_enum.py, but judges every same-file Python calls/references edge
the base has and the head lacks with linejudge.judge, which, unlike
reforacle.classify, tells same-named definitions apart by line and reads a
def's header in the scope around it. "unmatched": no occurrence of the name
is attributed to the source node (two same-named sources, say)."""
import importlib.util, json, os, random, shutil, sqlite3, subprocess, sys
from collections import Counter
from multiprocessing import Pool
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from linejudge import judge  # noqa: E402
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
ROWS = """SELECT e.relation, sf.path, sn.type, sn.name, COALESCE(sn.qualified_name, sn.name), sn.start_line, sn.end_line,
 tf.path, tn.type, tn.name, COALESCE(tn.qualified_name, tn.name), tn.start_line, tn.end_line,
 e.confidence, COALESCE(e.metadata,''), sf.language
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
BASE, HEAD, GEN, FIRST, COUNT, STEPS, WORK = sys.argv[1:8]
JOBS = int(sys.argv[8]) if len(sys.argv) > 8 else 6
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
    rows = list(con.execute(ROWS)); con.close()
    return rows

def seed(s):
    rng = random.Random(s); st = gen.initial(rng)
    v = Counter(); samples = []
    d = os.path.join(WORK, f"j{s}")
    for i in range(int(STEPS) + 1):
        if i:
            st = gen.step(st, rng)
        b = index(BASE, st, d + "b"); h = index(HEAD, st, d + "h")
        if b is None or h is None:
            v["error"] += 1; continue
        hk = Counter(tuple(r) for r in h)
        corpus = None
        for r in b:
            if hk[tuple(r)] > 0:
                hk[tuple(r)] -= 1; continue
            rel, sp, stp, sn, sq, s0, s1, tp, tt, tn, tq, t0, t1, conf, meta, lang = r
            if lang != "python" or rel not in ("calls", "references"):
                v[(rel, "other")] += 1; continue
            if tp != sp:
                v[(rel, "cross-file")] += 1; continue
            info = ""
            try:
                verdict = judge(st[sp], rel, 0 if stp == "module" else s0, tn, t0)
            except SyntaxError:
                verdict = "parse-fail"
            v[(rel, verdict)] += 1
            if verdict == "unmatched":
                import ast as _a
                names = [n.name for n in _a.walk(_a.parse(st[sp])) if isinstance(n, (_a.FunctionDef, _a.AsyncFunctionDef))]
                verdict = "unmatched-dup-source" if names.count(sn) > 1 else "unmatched-other"
            if verdict in ("right", "unmatched-other") and len(samples) < 3:
                samples.append({"seed": s, "step": i, "edge": f"{rel} {sp}:{sq}@{s0} -> {tp}:{tq}@{t0}", "info": str(info)})
    shutil.rmtree(d + "b", ignore_errors=True); shutil.rmtree(d + "h", ignore_errors=True)
    return v, samples

if __name__ == "__main__":
    os.makedirs(WORK, exist_ok=True)
    tot = Counter(); allsamples = []
    with Pool(JOBS) as pool:
        for v, smp in pool.imap_unordered(seed, range(int(FIRST), int(FIRST) + int(COUNT)), chunksize=4):
            tot.update(v); allsamples += smp
    print(json.dumps({str(k): n for k, n in sorted(tot.items(), key=str)}))
    for x in allsamples[:30]:
        print("SAMPLE", json.dumps(x))
