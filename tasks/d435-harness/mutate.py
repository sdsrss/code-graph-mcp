#!/usr/bin/env python3
"""Apply one mutation at a time to a scratch tree, run one test, restore.

usage: mutate.py <tree> <test-name> <mutations.json>
mutations.json: [{"id":..., "file": rel path, "old": text, "new": text}, ...]
A mutation whose `old` text is not found exactly once is reported, not run.
"""
import json, os, subprocess, sys
tree, test, muts = sys.argv[1], sys.argv[2], json.load(open(sys.argv[3]))
env = dict(os.environ, CARGO_TARGET_DIR="/var/tmp/backlog/b4/target-mut")
def run():
    r = subprocess.run(["cargo", "test", "--lib", test], cwd=tree, env=env,
                       capture_output=True, text=True)
    out = r.stdout + r.stderr
    panic = next((l for l in out.splitlines() if "panicked at" in l), "")
    detail = ""
    if panic:
        lines = out.splitlines()
        i = lines.index(panic)
        detail = " | ".join(lines[i + 1:i + 3])[:300]
    ok = "test result: ok. 1 passed" in out
    return r.returncode, ok, panic, detail, ("error[" in out)
for m in [{"id": "control", "file": None}] + muts:
    path = os.path.join(tree, m["file"]) if m["file"] else None
    orig = open(path).read() if path else None
    if path:
        if orig.count(m["old"]) != 1:
            print(json.dumps({"id": m["id"], "skipped": f"old text found {orig.count(m['old'])}x"}))
            continue
        open(path, "w").write(orig.replace(m["old"], m["new"], 1))
    try:
        rc, ok, panic, detail, compile_err = run()
    finally:
        if path:
            open(path, "w").write(orig)
    print(json.dumps({"id": m["id"], "rc": rc, "passed": ok, "compile_error": compile_err,
                      "panic": panic.split("panicked at")[-1].strip(), "detail": detail}), flush=True)
