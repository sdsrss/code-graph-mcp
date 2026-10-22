#!/usr/bin/env python3
"""cg.py <dir> <bin> <args...>: run a code-graph binary in <dir> under the sandbox HOME.
Prints wall and CPU (user+sys of the child) seconds to stderr when CG_TIME=1."""
import os, resource, subprocess, sys, time
d, b, *args = sys.argv[1:]
home = "/var/tmp/d370a-review2/fxhome"; os.makedirs(home, exist_ok=True)
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
b = b if "/" in b else os.path.join("/var/tmp/d370a-review2/bin", b)
t = time.time(); r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
rc = subprocess.run([b] + args, cwd=d, env=env).returncode
r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
if os.environ.get("CG_TIME"):
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    print(f"TIME wall={time.time()-t:.3f} cpu={cpu:.3f}", file=sys.stderr)
sys.exit(rc)
