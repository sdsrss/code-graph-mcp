#!/usr/bin/env python3
"""timeab.py <runs> <case-dir>...: per case and run, a fresh copy per arm,
alternating base/head (order flipped every run), wall seconds of
incremental-index; prints per-run times and means."""
import os, shutil, subprocess, sys, time, statistics

BIN = {"base": "/var/tmp/d909-review/bin/base", "head": "/var/tmp/d909-review/bin/head"}
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
TW = "/var/tmp/d909-review/tw"

runs = int(sys.argv[1])
for case in sys.argv[2:]:
    name = os.path.basename(case.rstrip("/"))
    t = {"base": [], "head": []}
    for i in range(runs):
        order = ["base", "head"] if i % 2 == 0 else ["head", "base"]
        for arm in order:
            d = os.path.join(TW, f"{name}-{arm}")
            assert d.startswith("/var/tmp/d909-review/")
            shutil.rmtree(d, ignore_errors=True)
            shutil.copytree(case, d, symlinks=True, ignore=shutil.ignore_patterns(".code-graph"))
            subprocess.run(["git", "init", "-q"], cwd=d, check=True)
            s = time.monotonic()
            r = subprocess.run([BIN[arm], "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV,
                               stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True, timeout=1200)
            e = time.monotonic() - s
            if r.returncode:
                print(name, arm, "rc", r.returncode, r.stderr[-300:])
            t[arm].append(e)
            shutil.rmtree(d, ignore_errors=True)
    mb, mh = statistics.mean(t["base"]), statistics.mean(t["head"])
    print(f"{name}: base {[round(x, 2) for x in t['base']]} mean {mb:.2f}  head {[round(x, 2) for x in t['head']]} mean {mh:.2f}  ratio {mh / mb:.3f}", flush=True)
