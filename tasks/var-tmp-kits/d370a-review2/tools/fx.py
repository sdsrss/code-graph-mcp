#!/usr/bin/env python3
"""fx.py <fixture.txt> [bins...]: fixture file = blocks '=== path' followed by content.
Builds <work>/<name>/<bin>/ per binary, git init, full index, dumps python edges (calls/imports) and diffs."""
import os, shutil, subprocess, sys, difflib
fx = sys.argv[1]
bins = sys.argv[2:] or ["base", "head"]
B = "/var/tmp/d370a-review2/bin"
work = "/var/tmp/d370a-review2/fx/" + os.path.basename(fx).rsplit('.',1)[0]
home = "/var/tmp/d370a-review2/fxhome"; os.makedirs(home, exist_ok=True)
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
files = {}; cur = None
for line in open(fx):
    if line.startswith("=== "):
        cur = line[4:].strip(); files[cur] = ""
    elif cur is not None:
        files[cur] += line
outs = {}
for b in bins:
    d = os.path.join(work, b)
    if os.path.exists(d): shutil.rmtree(d)
    for p, c in files.items():
        fp = os.path.join(d, p); os.makedirs(os.path.dirname(fp), exist_ok=True)
        open(fp, "w").write(c)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([os.path.join(B, b), "incremental-index", "--no-embed", "--json"], cwd=d, env=env, capture_output=True, text=True)
    if r.returncode: print("FAIL", b, r.stderr[-2000:]); sys.exit(2)
    o = subprocess.run([sys.executable, "-I", "/var/tmp/d370a-review2/tools/dump.py", d, "--py", "--pending"], capture_output=True, text=True).stdout
    lines = [l for l in o.splitlines() if ("-calls[" in l or "-imports[" in l or "-references[" in l or l.startswith("PENDING"))]
    outs[b] = lines
if len(bins) == 2:
    a, b = bins
    sa, sb = set(outs[a]), set(outs[b])
    for l in sorted(sa | sb):
        tag = "  " if (l in sa and l in sb) else ("-A" if l in sa else "+B")
        if '--all' in os.environ.get('FXOPT','') or tag != "  ":
            print(tag, l)
        elif os.environ.get('FXOPT') == 'calls' and ('-calls[' in l):
            print(tag, l)
else:
    for l in outs[bins[0]]: print(l)
