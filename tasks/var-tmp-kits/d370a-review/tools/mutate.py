#!/usr/bin/env python3
"""mutate.py <name> <relfile> <old> <new> [test-filter]: apply one textual mutation to the mut/ copy (must match once),
run cargo test --lib <filter>, report result, restore the file from the pristine head/ copy."""
import os, subprocess, sys, shutil, re
name, rel, old, new = sys.argv[1:5]
flt = sys.argv[5] if len(sys.argv) > 5 else "indexer::pipeline"
M = "/var/tmp/d370a-review/mut"; H = "/var/tmp/d370a-review/head"
p = os.path.join(M, rel)
src = open(p).read()
assert src == open(os.path.join(H, rel)).read(), "mut copy not pristine"
n = src.count(old)
assert n == 1, f"{name}: old text matches {n} times"
open(p, "w").write(src.replace(old, new))
try:
    env = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d370a-review/target-head")
    r = subprocess.run(["cargo", "test", "--lib", flt], cwd=M, env=env, capture_output=True, text=True)
    out = r.stdout + r.stderr
    if "error[" in out or "error:" in out and "test result" not in out:
        print(f"[{name}] COMPILE ERROR\n" + "\n".join(l for l in out.splitlines() if 'error' in l)[:2000]); sys.exit(3)
    res = [l for l in out.splitlines() if l.startswith("test result")]
    fails = [l.split()[1] for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
    print(f"[{name}] {res[-1] if res else 'no result'}")
    for f in fails: print("   FAILED", f)
finally:
    shutil.copyfile(os.path.join(H, rel), p)
    assert open(p).read() == open(os.path.join(H, rel)).read()
