import os, random, re, shutil, subprocess, sys
bin_, corpus, runs, seed = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
W = os.path.join(os.environ["CG_WALK_WORK"], f"realinc-{corpus}-{seed}")
EDGES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "edges.sh")
if os.path.exists(W): shutil.rmtree(W)
shutil.copytree(os.path.join(os.environ["CG_CORPORA"], corpus), W, symlinks=True)
shutil.rmtree(f"{W}/.code-graph", ignore_errors=True)
idx = lambda: subprocess.run([bin_, "incremental-index", "--no-embed", "--quiet"], cwd=W, capture_output=True, timeout=1200)
dump = lambda: subprocess.run([EDGES, W], capture_output=True, text=True).stdout
idx(); e0 = dump()
files = subprocess.run(["git", "ls-files"], cwd=W, capture_output=True, text=True).stdout.split()
js = [f for f in files if re.search(r"\.(m|c)?[jt]sx?$", f) and os.path.isfile(f"{W}/{f}")]
reexp = re.compile(r"export\s*(\*|\{[^}]*\})\s*from")
barrels = [f for f in js if reexp.search(open(f"{W}/{f}", errors="replace").read())]
stems = {os.path.splitext(os.path.basename(b))[0] if not b.endswith(("index.js","index.ts","index.mjs","index.tsx")) else os.path.basename(os.path.dirname(b)) for b in barrels}
importers = [f for f in js if f not in barrels and any(re.search(r"from\s+['\"][^'\"]*/" + re.escape(s) + r"(\.[mc]?[jt]sx?)?['\"]", open(f"{W}/{f}", errors="replace").read()) for s in list(stems)[:200])]
rng = random.Random(seed)
diffs = 0
for r in range(runs):
    pick = rng.sample(barrels, min(len(barrels), rng.randint(1, 3))) + rng.sample(importers, min(len(importers), rng.randint(0, 2)))
    for f in pick:
        with open(f"{W}/{f}", "a") as h: h.write("\n// e\n")
    idx(); e = dump()
    if e != e0:
        diffs += 1
        a, b = set(e0.splitlines()), set(e.splitlines())
        print(f"run {r} DIFF picked={pick} incremental-only={len(b-a)} rebuild-only={len(a-b)}", flush=True)
        for l in sorted(b - a)[:3]: print("   +", l[:200])
        for l in sorted(a - b)[:3]: print("   -", l[:200])
print(f"{corpus}: barrels={len(barrels)} importers={len(importers)} runs={runs} diff_runs={diffs} edges={len(e0.splitlines())}")
shutil.rmtree(W)
