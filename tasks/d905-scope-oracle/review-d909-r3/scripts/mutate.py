#!/usr/bin/env python3
"""mutate.py [ids...]: apply each mutant to src/mut, build lib tests, run them, restore."""
import os, re, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from mutants import MUTANTS
W = "/var/tmp/agent-tmp-d909-review3"; SRC = f"{W}/src/mut"; ORIG = f"{W}/src/f06f5e96"
ENV = dict(os.environ, CARGO_TARGET_DIR=f"{W}/tgt/mut")
pick = sys.argv[1:]
log = open(f"{W}/logs/mutants.txt", "a")
for name, path, old, new in MUTANTS:
    if pick and name.split()[0] not in pick:
        continue
    f = os.path.join(SRC, path)
    shutil.copy2(os.path.join(ORIG, path), f); os.utime(f)
    s = open(f).read()
    n = s.count(old)
    if n != 1:
        print(f"{name}: SKIP pattern count {n}", file=log, flush=True); continue
    open(f, "w").write(s.replace(old, new)); os.utime(f)
    t = time.time()
    b = subprocess.run(["cargo", "test", "--release", "--locked", "--features", "embed-model", "--lib", "-j", "4", "--no-run"],
                       cwd=SRC, env=ENV, capture_output=True, text=True)
    if b.returncode:
        err = [l for l in b.stderr.splitlines() if l.startswith("error")][:3]
        print(f"{name}: BROKEN (no compile) {err}", file=log, flush=True)
    else:
        r = subprocess.run(["cargo", "test", "--release", "--locked", "--features", "embed-model", "--lib", "-j", "4", "--", "--test-threads", "4"],
                           cwd=SRC, env=ENV, capture_output=True, text=True)
        res = re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed", r.stdout)
        fails = re.findall(r"^    (\S+)$", r.stdout.split("failures:")[-1], re.M) if "failures:" in r.stdout else []
        verdict = "KILLED" if res and int(res[-1][1]) > 0 else ("SURVIVED" if res else "NO-RESULT")
        print(f"{name}: {verdict} result={res} secs={time.time()-t:.0f} failing={fails[:8]}", file=log, flush=True)
        open(f"{W}/logs/mut-{name.split()[0]}.out", "w").write(r.stdout[-20000:])
    shutil.copy2(os.path.join(ORIG, path), f); os.utime(f)
print("MUTATION DONE", file=log, flush=True)
