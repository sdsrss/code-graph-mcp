#!/usr/bin/env python3
"""cost.py full|edit ...: alternating base/head measurements, medians.
full <corpus> <name> <reps>: fresh full index CPU (user+sys of the child) and wall.
edit <name> <reps> <relpath>...: in pre-indexed /var/tmp/d520-review/cost/<name>/<arm>, append a comment to
each file in turn and time `incremental-index` (wall + CPU); 'NEW' = add then delete a new module."""
import os, sys, subprocess, shutil, time, resource, statistics
R = "/var/tmp/d520-review"
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
ARMS = os.environ.get("ARMS", "base,head").split(",")
def run(arm, d):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t0 = time.perf_counter()
    r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
    t1 = time.perf_counter(); r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    assert r.returncode == 0, r.stderr[-800:]
    return t1 - t0, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
mode = sys.argv[1]
if mode == "full":
    corpus, name, reps = sys.argv[2], sys.argv[3], int(sys.argv[4])
    src = f"{R}/cost/{name}-src"
    if not os.path.exists(src):
        os.makedirs(src); subprocess.run(f"git -C {corpus} archive HEAD | tar -x -C {src}", shell=True, check=True)
    res = {a: [] for a in ARMS}
    for i in range(reps):
        for a in (ARMS if i % 2 == 0 else ARMS[::-1]):
            d = f"{R}/cost/{name}-full-{a}"
            if os.path.exists(d): shutil.rmtree(d)
            shutil.copytree(src, d, symlinks=True); subprocess.run(["git", "init", "-q", d], check=True)
            res[a].append(run(a, d))
    for a in ARMS:
        w = [x[0] for x in res[a]]; c = [x[1] for x in res[a]]
        print(f"full {name} {a}: cpu median {statistics.median(c):.2f}s (runs {', '.join(f'{x:.2f}' for x in c)}) wall median {statistics.median(w):.2f}s")
    mb, mh = statistics.median(x[1] for x in res[ARMS[0]]), statistics.median(x[1] for x in res[ARMS[1]])
    print(f"full {name}: cpu {ARMS[1]}/{ARMS[0]} = {mh:.2f}/{mb:.2f} = {mh/mb:.3f}")
else:
    name, reps = sys.argv[2], int(sys.argv[3]); paths = sys.argv[4:]
    for p in paths:
        res = {a: [] for a in ARMS}
        for i in range(reps):
            for a in (ARMS if i % 2 == 0 else ARMS[::-1]):
                d = f"{R}/cost/{name}/{a}"
                if p == "NEW":
                    fp = os.path.join(d, "zz_cost_new_module.py")
                    open(fp, "w").write("def zz_cost_fn():\n    return 1\n")
                    res[a].append(run(a, d)); os.remove(fp); run(a, d)
                else:
                    fp = os.path.join(d, p); open(fp, "a").write(f"\n# cost {i}\n")
                    res[a].append(run(a, d))
        mb = statistics.median(x[0] for x in res[ARMS[0]]); mh = statistics.median(x[0] for x in res[ARMS[1]])
        cb = statistics.median(x[1] for x in res[ARMS[0]]); ch = statistics.median(x[1] for x in res[ARMS[1]])
        print(f"edit {name} {p}: wall median {ARMS[0]} {mb*1000:.0f} ms {ARMS[1]} {mh*1000:.0f} ms ({mh/mb:.3f}); cpu {cb*1000:.0f} vs {ch*1000:.0f} ms; runs {[round(x[0]*1000) for x in res[ARMS[0]]]} vs {[round(x[0]*1000) for x in res[ARMS[1]]]}")
