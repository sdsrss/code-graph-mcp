"""inctime.py <corpus> <n> <op> <args> -- bins...: per arm, full index once; then n alternating trials:
copy the indexed tree, apply the op, time incremental-index. Prints per-arm wall ms and files re-extracted."""
import sys, os, shutil, subprocess, time, statistics, re, json
sys.path.insert(0, "/var/tmp/d520/harness")
from corp import *
sep = sys.argv.index("--")
corpus, n, op, args, bins = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4:sep], sys.argv[sep+1:]
base = {}
for b in bins:
    w = f"{R}/work/inctime/{os.path.basename(corpus)}/{os.path.basename(b)}/base"
    copy(corpus, w); index(b, w); base[b] = w
res = {b: [] for b in bins}; files = {b: set() for b in bins}
for i in range(n):
    for b in (bins if i % 2 == 0 else bins[::-1]):
        t = base[b] + f"-t"
        if os.path.exists(t): shutil.rmtree(t)
        subprocess.run(["cp", "-a", base[b], t], check=True)
        p = os.path.join(t, args[0])
        if op == "create": open(p, "w").write(args[1].encode().decode("unicode_escape"))
        elif op == "delete": os.remove(p)
        elif op == "append": open(p, "a").write(args[1].encode().decode("unicode_escape"))
        t0 = time.time()
        r = subprocess.run([b, "incremental-index", "--no-embed", "--json"], cwd=t, env=env, capture_output=True, text=True)
        res[b].append((time.time() - t0) * 1000)
        try: files[b].add(json.loads(r.stdout).get("files_indexed"))
        except Exception: files[b].add(r.stdout[:80])
for b in bins:
    v = sorted(res[b]); print(f"{os.path.basename(b)}: median {statistics.median(v):.0f} ms  min {v[0]:.0f} max {v[-1]:.0f}  files {files[b]}")
