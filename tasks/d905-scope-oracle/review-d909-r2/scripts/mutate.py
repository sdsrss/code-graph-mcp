#!/usr/bin/env python3
"""mutate.py <mutants.py> [substr...]: apply each mutant to /var/tmp/d909-review2/mut-src,
cargo test --lib, record compile status / passed / failed / failing names, restore + touch."""
import importlib.util, os, re, subprocess, sys, time, json
SRC = "/var/tmp/d909-review2/mut-src"
ENV = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/d909-review2/target-mut")
spec = importlib.util.spec_from_file_location("m", sys.argv[1]); mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
pick = sys.argv[2:]
out = open("/var/tmp/d909-review2/mutants.jsonl", "a")
for name, (f, old, new) in mod.MUTANTS.items():
    if pick and not any(p in name for p in pick):
        continue
    path = os.path.join(SRC, f)
    orig = open(path).read()
    n = orig.count(old)
    if n != 1:
        print(f"{name}: anchor count {n}, skipped", flush=True); continue
    open(path, "w").write(orig.replace(old, new))
    t = time.time()
    try:
        r = subprocess.run(["cargo", "test", "--locked", "--lib", "--no-fail-fast"], cwd=SRC, env=ENV, capture_output=True, text=True)
    finally:
        open(path, "w").write(orig)
        os.utime(path, None)
    log = r.stdout + r.stderr
    compiled = "test result:" in log
    m = re.search(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored", log)
    fails = sorted(set(re.findall(r"^    ([\w:]+)$", log, re.M)))
    rec = {"mutant": name, "compiled": compiled, "exit": r.returncode, "passed": m and int(m.group(1)), "failed": m and int(m.group(2)), "fails": fails, "secs": round(time.time() - t)}
    if not compiled:
        rec["err"] = log[-1500:]
    print(json.dumps(rec), flush=True)
    out.write(json.dumps(rec) + "\n"); out.flush()
