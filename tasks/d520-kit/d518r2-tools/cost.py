#!/usr/bin/env python3
"""cost.py <corpus-src> <name> <reps> <relpath|NEW>...: for each arm (ARMS env), index a
private copy once; then per edit, <reps> rounds with the arm order rotated each round:
append a comment to the file (or add a new module, timed, then delete it, untimed) and
time `incremental-index` (wall + child CPU). Prints medians and the load average."""
import os, sys, subprocess, shutil, time, resource, statistics, json
R = "/var/tmp/d518r2"
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
ARMS = os.environ.get("ARMS", "pre,base,head").split(",")
src, name, reps, paths = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4:]
def run(arm, d):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t0 = time.perf_counter()
    r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed", "--json"], cwd=d, env=env, capture_output=True, text=True)
    t1 = time.perf_counter(); r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    assert r.returncode == 0, r.stderr[-800:]
    j = json.loads([l for l in r.stdout.splitlines() if l.startswith("{")][-1])
    return t1 - t0, (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime), j["files_indexed"]
dirs = {}
for a in ARMS:
    d = f"{R}/cost/{name}/{a}"
    if os.path.exists(d): shutil.rmtree(d)
    os.makedirs(d)
    subprocess.run(f"git -C {src} archive HEAD | tar -x -C {d}", shell=True, check=True)
    subprocess.run(["git", "init", "-q", d], check=True)
    run(a, d); dirs[a] = d
print(f"[{name}] load before: {open('/proc/loadavg').read().split()[:3]}", flush=True)
for p in paths:
    res = {a: [] for a in ARMS}; files = {a: set() for a in ARMS}
    for i in range(reps):
        k = i % len(ARMS); order = ARMS[k:] + ARMS[:k]
        for a in order:
            d = dirs[a]
            if p == "NEW":
                fp = os.path.join(d, "zz_cost_new_module.py")
                open(fp, "w").write("def zz_cost_fn():\n    return 1\n")
                w, c, f = run(a, d); os.remove(fp); run(a, d)
            else:
                open(os.path.join(d, p), "a").write(f"\n# cost {i}\n")
                w, c, f = run(a, d)
            res[a].append((w, c)); files[a].add(f)
    line = [f"edit {name} {p} (n={reps}):"]
    for a in ARMS:
        line.append(f"{a} wall {statistics.median(x[0] for x in res[a])*1000:.0f} ms cpu {statistics.median(x[1] for x in res[a])*1000:.0f} ms files {sorted(files[a])}")
    print("  ".join(line), flush=True)
    for a in ARMS:
        print(f"    {a} wall runs {[round(x[0]*1000) for x in res[a]]}", flush=True)
print(f"[{name}] load after: {open('/proc/loadavg').read().split()[:3]}", flush=True)
for a in ARMS: shutil.rmtree(dirs[a])
