#!/usr/bin/env python3
"""dynjudge.py <gen.py> <first> <count> <steps> <work> <jobs> <arm>... (last arm = head)
For each generated state: CPython call trace (dyn_run.py) + a fresh index per arm.
Every `calls` edge an earlier arm has and the head lacks is judged right when CPython
made that call (callee = target def; call-site line inside the source node, decorator
lines counted to the scope around). Also: CPython call pairs each arm binds (recall)."""
import importlib.util, json, os, random, shutil, sqlite3, subprocess, sys
from collections import Counter
from multiprocessing import Pool
R = "/var/tmp/agent-tmp-d909-review3"
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
GEN, FIRST, COUNT, STEPS, WORK, JOBS = sys.argv[1:7]; ARMS = sys.argv[7:]
spec = importlib.util.spec_from_file_location("gen", GEN); gen = importlib.util.module_from_spec(spec); spec.loader.exec_module(gen)
NODES = "SELECT n.id, f.path, n.name, n.type, n.start_line, n.end_line FROM nodes n JOIN files f ON f.id=n.file_id"
EDGES = "SELECT source_id, target_id FROM edges WHERE relation='calls'"

def write(files, d):
    assert d.startswith(R + "/")
    shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
    for p, body in files.items():
        f = os.path.join(d, p); os.makedirs(os.path.dirname(f), exist_ok=True); open(f, "w").write(body)

def index(arm, files, d):
    write(files, d); subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([f"{R}/bin/{arm}", "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    if r.returncode:
        return None
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    nodes = {i: (p, None, n, t, s, e) for i, p, n, t, s, e in con.execute(NODES)}
    edges = set()
    for s, t in con.execute(EDGES):
        edges.add((nodes[s], nodes[t]))
    con.close()
    return nodes, edges

def dyn(files, d):
    write(files, d)
    out = d + ".json"
    try:
        subprocess.run([sys.executable, "-I", f"{R}/scripts/dyn_run.py", d, out], timeout=30, capture_output=True)
        ev = json.load(open(out))
    except Exception:
        ev = None
    shutil.rmtree(d, ignore_errors=True)
    if os.path.exists(out): os.remove(out)
    return ev

def def_line(lines, start):
    i = start
    while i <= len(lines) and lines[i - 1].lstrip().startswith("@"):
        i += 1
    return i

def map_events(ev, nodes, files):
    """-> set of (source node tuple, target node tuple) CPython made; source candidates."""
    by_file = {}
    for nid, nd in nodes.items():
        by_file.setdefault(nd[0], []).append(nd)
    pairs = set()
    for bf, bl, cf, cl, name, qn in ev:
        if name == "__init__":
            cls = qn.split(".")[-2] if "." in qn else None
            tg = [n for n in by_file.get(cf, []) if n[2] == cls and n[3] == "class" and n[3] == "class" and n[4] == cl - 1]
        else:
            tg = [n for n in by_file.get(cf, []) if n[2] == name and n[4] == cl and n[3] in ("function", "method")]
        if len(tg) != 1:
            continue
        lines = files[bf].split("\n")
        deco = lines[bl - 1].lstrip().startswith("@") if 0 < bl <= len(lines) else False
        cands = [n for n in by_file.get(bf, []) if n[3] in ("function", "method") and n[4] <= bl <= n[5]
                 and not (deco and bl < def_line(lines, n[4]))]
        if cands:
            src = min(cands, key=lambda n: n[5] - n[4])
            srcs = [src]
        else:
            srcs = [n for n in by_file.get(bf, []) if n[2] == "<module>" or (n[3] == "class" and n[4] <= bl <= n[5])]
        for s in srcs:
            pairs.add((s, tg[0]))
    return pairs

def seed(s):
    rng = random.Random(s); st = gen.initial(rng)
    c = Counter(); samples = []
    d = os.path.join(WORK, f"d{s}")
    for i in range(int(STEPS) + 1):
        if i:
            st = gen.step(st, rng)
        ev = dyn(st, d + "x")
        res = {a: index(a, st, d + a.replace(".", "")) for a in ARMS}
        if ev is None or any(v is None for v in res.values()):
            c["error"] += 1; continue
        c["states"] += 1
        head = ARMS[-1]
        for a in ARMS:
            pairs = map_events(ev, res[a][0], st)
            c[("dyn-pairs", a)] += len(pairs)
            c[("dyn-bound", a)] += len(pairs & res[a][1])
        hp = map_events(ev, res[head][0], st)
        for a in ARMS[:-1]:
            ap = map_events(ev, res[a][0], st)
            lost = res[a][1] - res[head][1]
            for e in lost:
                same = e[0][0] == e[1][0]
                ok = e in ap
                c[(a, "lost", "right" if ok else "not-seen", "same-file" if same else "cross-file")] += 1
                if ok and len(samples) < 8:
                    samples.append({"seed": s, "step": i, "arm": a, "edge": f"{e[0][0]}:{e[0][2]}@{e[0][4]} -> {e[1][0]}:{e[1][2]}@{e[1][4]}({e[1][3]})"})
            gained = res[head][1] - res[a][1]
            c[(a, "head-gained")] += len(gained)
    for a in ARMS:
        shutil.rmtree(d + a.replace(".", ""), ignore_errors=True)
    return c, samples

if __name__ == "__main__":
    os.makedirs(WORK, exist_ok=True)
    tot = Counter(); allsamples = []
    with Pool(int(JOBS)) as pool:
        for c, smp in pool.imap_unordered(seed, range(int(FIRST), int(FIRST) + int(COUNT)), chunksize=2):
            tot.update(c); allsamples += smp
    for k, v in sorted(tot.items(), key=str):
        print(k, v)
    for x in allsamples[:40]:
        print("SAMPLE", json.dumps(x))
