import os, shutil, subprocess, sys, time
sys.path.insert(0, "/var/tmp/d905-review/tools")
from mutants import MUTANTS, MUTANTS2
if "--b2" in sys.argv: MUTANTS = MUTANTS2; sys.argv.remove("--b2")
M = "/var/tmp/d905-review/mut-src"; H = "/var/tmp/d905-review/head-src"
ENV = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d905-review/target-mut")
only = sys.argv[1:]
log = open("/var/tmp/d905-review/mutants.log", "a")
for mid, f, old, new, desc in MUTANTS:
    if only and mid not in only: continue
    path = os.path.join(M, f); orig = open(os.path.join(H, f)).read()
    assert open(path).read() == orig, f"{mid}: copy not pristine"
    n = orig.count(old)
    if n != 1:
        print(f"{mid} SKIP old string count {n}", file=log, flush=True); continue
    open(path, "w").write(orig.replace(old, new)); os.utime(path)
    t = time.time()
    r = subprocess.run(["cargo", "test", "--lib", "--", "--test-threads=8"], cwd=M, env=ENV, capture_output=True, text=True)
    out = r.stdout + r.stderr
    res = [l for l in out.splitlines() if l.startswith("test result")]
    fails = [l.split()[1] for l in out.splitlines() if l.startswith("test ") and l.endswith("FAILED")]
    compile_err = "error[" in out or "could not compile" in out
    shutil.copyfile(os.path.join(H, f), path); os.utime(path)
    assert open(path).read() == orig
    verdict = "COMPILE-ERROR" if compile_err else ("KILLED" if fails else ("SURVIVED" if res else "NO-RESULT"))
    print(f"{mid} {verdict} {desc} | {res[-1] if res else ''} | failed={fails[:6]} | {time.time()-t:.0f}s", file=log, flush=True)
