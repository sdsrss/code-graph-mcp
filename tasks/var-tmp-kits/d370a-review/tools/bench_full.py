#!/usr/bin/env python3
"""bench_full.py <rounds> <dirA> <binA> <dirB> <binB>: alternate full indexes (fresh .code-graph each), print wall/cpu."""
import os, shutil, subprocess, sys, time, resource, statistics
n = int(sys.argv[1]); pairs = [(sys.argv[2], sys.argv[3]), (sys.argv[4], sys.argv[5])]
home = "/var/tmp/d370a-review/fxhome"
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
res = {b: [] for _, b in pairs}
for i in range(n):
    for d, b in (pairs if i % 2 == 0 else pairs[::-1]):
        cg = os.path.join(d, ".code-graph")
        assert d.startswith("/var/tmp/d370a-review/")
        if os.path.isdir(cg): shutil.rmtree(cg)
        r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
        r = subprocess.run([b, "incremental-index", "--no-embed", "--json"], cwd=d, env=env, capture_output=True, text=True)
        w = (time.time() - t) * 1000; r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        assert r.returncode == 0, r.stderr[-500:]
        res[b].append((w, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)))
for b, v in res.items():
    ws = sorted(x[0] for x in v); cs = sorted(x[1] for x in v)
    print(f"{os.path.basename(b)}: wall ms median {statistics.median(ws):.0f} range {ws[0]:.0f}-{ws[-1]:.0f}; cpu s median {statistics.median(cs):.2f} range {cs[0]:.2f}-{cs[-1]:.2f}")
