#!/usr/bin/env python3
"""stress.py <N> <shape>: write a corpus under /var/tmp/d370a-review2/stress/<shape>-<N>/{base,head}
and time a full index with each binary (3 alternating runs, CPU).
shapes: flat  - big.py with N top-level defs; caller.py calls big.f_i() for all i
        cls   - big.py with N/10 classes x 10 methods + N top-level defs; same caller
        many  - N/50 caller files, each calling 50 distinct big.f_i()"""
import os, resource, shutil, statistics, subprocess, sys, time
N = int(sys.argv[1]); shape = sys.argv[2]
R = "/var/tmp/d370a-review2"
env = dict(os.environ, HOME=f"{R}/fxhome", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
def corpus():
    files = {}
    big = []
    if shape == "cls":
        for c in range(N // 10):
            big.append(f"class C{c}:\n" + "".join(f"    def f_{c*10+k}(self):\n        return {k}\n" for k in range(10)))
    big += [f"def f_{i}():\n    return {i}\n" for i in range(N)]
    files["pkg/__init__.py"] = ""
    files["pkg/big.py"] = "\n".join(big)
    if shape == "many":
        for j in range(N // 50):
            files[f"app/c{j}.py"] = "import pkg.big as big\n\ndef run():\n" + "".join(f"    big.f_{j*50+k}()\n" for k in range(50))
    else:
        files["app/caller.py"] = "import pkg.big as big\n\ndef run():\n" + "".join(f"    big.f_{i}()\n" for i in range(N))
    return files
base_dir = f"{R}/stress/{shape}-{N}"
res = {}
for b in ("base", "head"):
    d = os.path.join(base_dir, b)
    if not os.path.exists(d):
        for p, c in corpus().items():
            fp = os.path.join(d, p); os.makedirs(os.path.dirname(fp), exist_ok=True); open(fp, "w").write(c)
        subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    res[b] = []
for i in range(3):
    for b in ("base", "head") if i % 2 == 0 else ("head", "base"):
        d = os.path.join(base_dir, b); cg = os.path.join(d, ".code-graph")
        if os.path.exists(cg): shutil.rmtree(cg)
        r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
        r = subprocess.run([f"{R}/bin/{b}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
        r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
        if r.returncode: print("FAIL", r.stderr[-800:]); sys.exit(2)
        res[b].append(((r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime), time.time() - t))
for b, v in res.items():
    print(f"{shape} N={N} {b}: cpu median {statistics.median(x[0] for x in v):.2f} s, wall median {statistics.median(x[1] for x in v):.2f} s")
