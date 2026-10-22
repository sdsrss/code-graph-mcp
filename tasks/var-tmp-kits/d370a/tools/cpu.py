#!/usr/bin/env python3
"""cpu.py <binary> <dir> <home>: full index of <dir> (its .code-graph removed first); print child CPU seconds (user+sys) and wall ms."""
import os, resource, shutil, subprocess, sys, time
b, d, h = sys.argv[1:4]
shutil.rmtree(os.path.join(d, ".code-graph"), ignore_errors=True)
r0 = resource.getrusage(resource.RUSAGE_CHILDREN); t = time.time()
subprocess.run([b, "incremental-index", "--no-embed"], cwd=d, env=dict(os.environ, HOME=h), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
print(f"{(r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime):.2f} {int((time.time() - t) * 1000)}")
