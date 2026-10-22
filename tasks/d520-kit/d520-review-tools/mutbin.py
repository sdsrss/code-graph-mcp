#!/usr/bin/env python3
"""mutbin.py <id>...: build a release binary of each mutant into bin/cgm-mut<id>; restore the tree."""
import os, sys, subprocess, runpy, shutil
R = "/var/tmp/d520-review"; MUT = R + "/mut"; PRISTINE = R + "/head"
M = {m[0]: m for m in runpy.run_path(R + "/tools/mutants.py")["M"]}
env = dict(os.environ, PATH=os.environ["HOME"] + "/.cargo/bin:" + os.environ["PATH"], CARGO_TARGET_DIR=R + "/tgt-mutrel")
for mid in sys.argv[1:]:
    _, f, old, new, note = M[mid]
    src = open(f"{PRISTINE}/{f}").read(); assert src.count(old) == 1
    open(f"{MUT}/{f}", "w").write(src.replace(old, new))
    r = subprocess.run(["cargo", "build", "--release"], cwd=MUT, env=env, capture_output=True, text=True)
    shutil.copy(f"{PRISTINE}/{f}", f"{MUT}/{f}")
    assert open(f"{MUT}/{f}").read() == src
    if r.returncode: print(mid, "BUILD FAILED", r.stderr[-800:]); continue
    shutil.copy(R + "/tgt-mutrel/release/code-graph-mcp", f"{R}/bin/cgm-mut{mid}")
    print(mid, "built", subprocess.run(["md5sum", f"{R}/bin/cgm-mut{mid}"], capture_output=True, text=True).stdout.split()[0], flush=True)
