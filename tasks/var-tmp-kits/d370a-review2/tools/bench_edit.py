#!/usr/bin/env python3
"""bench_edit.py <dir> <bin> <relpath> <appended-text> <n>: on an indexed tree, append text to a
file, run incremental-index (timed, RUST_LOG=info for fan-out lines), restore the file, run
incremental-index again (timed). n repetitions. Prints per-run wall/cpu and fan-out lines."""
import os, resource, statistics, subprocess, sys, time
d, b, rel, text, n = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4].encode().decode('unicode_escape'), int(sys.argv[5])
home = "/var/tmp/d370a-review2/fxhome"
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1", RUST_LOG="info")
binary = os.path.join("/var/tmp/d370a-review2/bin", b)
fp = os.path.join(d, rel)
orig = open(fp).read()
def run(tag):
    r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
    r = subprocess.run([binary, "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
    wall = time.time() - t; r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    if r.returncode: print("FAIL", r.stderr[-1500:]); sys.exit(2)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    fan = [l.split("] ", 1)[-1][:200] for l in r.stderr.splitlines() if "fan-out round" in l]
    print(f"  {tag}: wall={wall:.3f} cpu={cpu:.3f} {fan}", flush=True)
    return wall
run("warm")
we, wr = [], []
for i in range(n):
    time.sleep(1.1)
    open(fp, "w").write(orig + text); t = time.time() + 2; os.utime(fp, (t, t))
    we.append(run(f"edit {i}"))
    time.sleep(1.1)
    open(fp, "w").write(orig); t = time.time() + 4; os.utime(fp, (t, t))
    wr.append(run(f"restore {i}"))
print(f"{b}: edit wall median {statistics.median(we):.3f} (min {min(we):.3f} max {max(we):.3f}); restore median {statistics.median(wr):.3f}")
