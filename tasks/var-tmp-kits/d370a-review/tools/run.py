#!/usr/bin/env python3
"""run.py <bin> <dir> [cli args...]: run the binary in <dir> under a sandbox HOME; print stdout, stderr tail,
wall ms and child CPU s (user+sys). Default args: incremental-index --no-embed --json."""
import os, subprocess, sys, time, resource
binary, d = sys.argv[1], sys.argv[2]
args = sys.argv[3:] or ["incremental-index", "--no-embed", "--json"]
home = "/var/tmp/d370a-review/fxhome"; os.makedirs(home, exist_ok=True)
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
r = subprocess.run([binary] + args, cwd=d, env=env, capture_output=True, text=True)
wall = (time.time() - t) * 1000; r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
sys.stdout.write(r.stdout)
if os.environ.get("SHOWERR"): sys.stdout.write(r.stderr[-int(os.environ.get("SHOWERR")):])
print(f"## rc={r.returncode} wall_ms={wall:.0f} cpu_s={cpu:.2f}")
