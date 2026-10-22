#!/usr/bin/env python3
"""probe.py: run the review probe tests on the pristine copy and on each surviving mutant."""
import os, re, shutil, subprocess, sys
sys.path.insert(0, "/var/tmp/d370a-review2/tools")
R = "/var/tmp/d370a-review2"; PRISTINE = f"{R}/src/head"; MUT = f"{R}/src/mut"
spec = {}
exec(open(f"{R}/tools/mutate.py").read().split("ids = sys.argv")[0], spec)
M = spec["M"]
env = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_TARGET_DIR=f"{R}/target-mut")
PT = "src/parser/relations/tests.rs"; IT = "src/indexer/pipeline/tests.rs"
def restore(f): shutil.copyfile(os.path.join(PRISTINE, f), os.path.join(MUT, f))
def add_probes():
    for f, p in ((PT, "probe_parser.rs"), (IT, "probe_pipeline.rs")):
        restore(f)
        open(os.path.join(MUT, f), "a").write(open(f"{R}/tools/{p}").read())
def run(tag):
    r = subprocess.run(["cargo", "test", "--lib", "--", "review_probe", "--nocapture", "--test-threads=1"], cwd=MUT, env=env, capture_output=True, text=True)
    lines = [l[l.index("PROBE"):] for l in r.stdout.splitlines() if "PROBE" in l]
    if r.returncode or not lines: print(tag, "FAILED", r.stderr[-1500:], r.stdout[-1500:])
    for l in lines: print(f"[{tag}] {l}")
add_probes()
run("pristine")
for mid in sys.argv[1:]:
    f, old, new = M[mid]
    restore(f); p = os.path.join(MUT, f); s = open(p).read(); assert s.count(old) == 1, mid
    open(p, "w").write(s.replace(old, new))
    run(mid)
    restore(f)
for f in (PT, IT): restore(f)
for f in {m[0] for m in M.values()} | {PT, IT}:
    assert open(os.path.join(MUT, f)).read() == open(os.path.join(PRISTINE, f)).read(), f
print("restored OK")
