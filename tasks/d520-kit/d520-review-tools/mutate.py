#!/usr/bin/env python3
import os, sys, subprocess, runpy, shutil, re, time
R = "/var/tmp/d520-review"; MUT = R + "/mut"; PRISTINE = R + "/head"
M = runpy.run_path(R + "/tools/mutants.py")["M"]
pick = sys.argv[1:]
env = dict(os.environ, PATH=os.environ["HOME"] + "/.cargo/bin:" + os.environ["PATH"], RUSTUP_HOME=os.environ["HOME"] + "/.rustup",
           CARGO_HOME=os.environ["HOME"] + "/.cargo", CARGO_TARGET_DIR=R + "/tgt-mut", HOME=R + "/home")
log = open(R + "/mutate.log", "a")
for mid, f, old, new, note in M:
    if pick and mid not in pick: continue
    src = open(f"{PRISTINE}/{f}").read()
    n = src.count(old)
    if n != 1:
        print(f"{mid}: SKIP old occurs {n}x", flush=True); continue
    open(f"{MUT}/{f}", "w").write(src.replace(old, new))
    t = time.time()
    r = subprocess.run(["cargo", "test", "--lib", "-q"], cwd=MUT, env=env, capture_output=True, text=True, timeout=3000)
    out = r.stdout + r.stderr
    shutil.copy(f"{PRISTINE}/{f}", f"{MUT}/{f}")
    assert open(f"{MUT}/{f}").read() == src
    if "error[" in out and "test result" not in out:
        verdict = "BUILD-ERROR " + "\n".join(l for l in out.splitlines() if l.startswith("error"))[:300]
    else:
        failed = re.findall(r"^    ([a-z_0-9:]+)$", out, re.M)
        m = re.search(r"test result: \w+\. (\d+) passed; (\d+) failed", out)
        verdict = (f"KILLED {m.group(2) if m else '?'} failed: " + ", ".join(x.rsplit('::', 1)[-1] for x in failed[:8])) if r.returncode else "SURVIVED"
        if not m: verdict += " | no result line; tail: " + " / ".join(out.strip().splitlines()[-6:])[:600]
        if m: verdict += f" (passed {m.group(1)})"
    line = f"{mid} [{note}]: {verdict}  ({time.time()-t:.0f}s)"
    print(line, flush=True); log.write(line + "\n"); log.flush()
