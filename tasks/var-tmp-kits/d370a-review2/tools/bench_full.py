#!/usr/bin/env python3
"""bench_full.py <corpus_dir_A> <binA> <corpus_dir_B> <binB> <n>: alternate fresh full
indexes (delete <dir>/.code-graph first) and print CPU (user+sys) and wall per run, then medians."""
import os, resource, shutil, statistics, subprocess, sys, time
dA, bA, dB, bB, n = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5])
home = "/var/tmp/d370a-review2/fxhome"
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
B = "/var/tmp/d370a-review2/bin"
res = {bA: [], bB: []}
for i in range(n):
    for d, b in ((dA, bA), (dB, bB)) if i % 2 == 0 else ((dB, bB), (dA, bA)):
        assert d.startswith("/var/tmp/d370a-review2/") and os.path.isdir(d)
        cg = os.path.join(d, ".code-graph")
        if os.path.exists(cg):
            shutil.rmtree(cg)
        r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
        r = subprocess.run([os.path.join(B, b), "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
        wall = time.time() - t; r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        if r.returncode:
            print("FAIL", b, r.stderr[-1000:]); sys.exit(2)
        cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
        res[b].append((cpu, wall))
        print(f"run {i} {b}: cpu={cpu:.3f} wall={wall:.3f}", flush=True)
for b, v in res.items():
    c = [x[0] for x in v]; w = [x[1] for x in v]
    print(f"{b}: cpu median {statistics.median(c):.3f} (min {min(c):.3f} max {max(c):.3f}); wall median {statistics.median(w):.3f}")
