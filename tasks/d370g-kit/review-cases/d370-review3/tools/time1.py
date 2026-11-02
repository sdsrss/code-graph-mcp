# time1.py BIN KIND K [TIMEOUT]: index one generated file in a fresh dir; print wall and child CPU seconds
import os, subprocess, sys, time, resource, shutil
R = "/var/tmp/d370-review3"
b, kind, k = sys.argv[1], sys.argv[2], int(sys.argv[3]); to = float(sys.argv[4]) if len(sys.argv) > 4 else 600
d = f"{R}/perf/{kind}-{k}-{b}"
if os.path.isdir(d): shutil.rmtree(d)
os.makedirs(d)
src = subprocess.check_output([sys.executable, "-I", f"{R}/tools/gen.py", kind, str(k)])
open(f"{d}/m.py", "wb").write(src)
subprocess.run(["git", "init", "-q"], cwd=d, check=True)
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
t0 = time.time(); r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
try:
    p = subprocess.run([f"{R}/bin/{b}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, timeout=to)
    rc = p.returncode
except subprocess.TimeoutExpired:
    rc = "TIMEOUT"
t1 = time.time(); r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
n = None
try:
    import sqlite3
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    n = con.execute("select count(*) from edges").fetchone()[0]
except Exception as e:
    n = f"n/a"
print(f"{b:9} {kind:7} k={k:<7} bytes={len(src):<8} rc={rc} wall={t1-t0:.2f}s user={r1.ru_utime-r0.ru_utime:.2f}s edges={n}", flush=True)
