#!/usr/bin/env python3
"""truth.py OUT TRUTH_JSON: run every OUT/f*.py with CPython and record which
traced function `t_call()` runs.

Per file: {"import_error": str|None, "ran": [tags appended by t_call()],
"exc": str|None}. extpkg/ and extstar.py are read from OUT's parent directory.
Each file runs in its own subprocess, so no file sees another's classes.
"""
import json
import os
import subprocess
import sys

CHILD = r'''
import json, runpy, sys
sys.path.insert(0, sys.argv[2])
out = {"import_error": None, "ran": [], "exc": None}
try:
    g = runpy.run_path(sys.argv[1], run_name="scenario")
except BaseException as e:
    out["import_error"] = f"{type(e).__name__}: {e}"
else:
    trace = g["TRACE"]
    trace.clear()
    try:
        g["t_call"]()
    except BaseException as e:
        out["exc"] = f"{type(e).__name__}: {e}"
    out["ran"] = list(trace)
print(json.dumps(out))
'''


def main():
    out_dir, dest = sys.argv[1], sys.argv[2]
    ext_root = os.path.dirname(os.path.abspath(out_dir))
    truth = {}
    for name in sorted(os.listdir(out_dir)):
        if not (name.startswith("f") and name.endswith(".py")):
            continue
        r = subprocess.run(
            [sys.executable, "-I", "-c", CHILD, os.path.join(out_dir, name), ext_root],
            capture_output=True, text=True, timeout=60,
        )
        try:
            truth[name] = json.loads(r.stdout.strip().splitlines()[-1])
        except (IndexError, json.JSONDecodeError):
            truth[name] = {"import_error": f"runner: {r.stderr[-300:]}", "ran": [], "exc": None}
    json.dump(truth, open(dest, "w"), indent=0, sort_keys=True)
    bad = sum(1 for t in truth.values() if t["import_error"])
    print(f"{len(truth)} files, {bad} fail to import")


if __name__ == "__main__":
    main()
