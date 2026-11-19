"""seq.py <binary> <seqfile.py> : seqfile defines STATES (list of dict path->content, None deletes).
Each step: incremental index of a working copy vs fresh index; prints diffs."""
import os, sys, shutil, tempfile, importlib.util
sys.path.insert(0, "/home/ai/dev/code-graph-mcp/tasks/d893-fuzz")
import fuzz_seq as F
from collections import Counter
binary = os.path.abspath(sys.argv[1])
spec = importlib.util.spec_from_file_location("s", sys.argv[2]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
states = []
cur = {}
for st in m.STATES:
    cur = dict(cur)
    for p, c in st.items():
        if c is None: cur.pop(p, None)
        else: cur[p] = c
    states.append(cur)
base = tempfile.mkdtemp(prefix="seq-", dir="/var/tmp/d905-review/fuzzwork")
inc = os.path.join(base, "inc"); os.makedirs(inc)
import subprocess
subprocess.run(["git", "init", "-q"], cwd=inc, check=True)
stamp = 1_700_000_000; prev = None; bad = 0
for i, st in enumerate(states):
    stamp += 10
    F.write_state(inc, st, prev, stamp); prev = st
    F.index(binary, inc)
    fresh = os.path.join(base, f"fresh{i}"); os.makedirs(fresh)
    subprocess.run(["git", "init", "-q"], cwd=fresh, check=True)
    F.write_state(fresh, st, None, stamp); F.index(binary, fresh)
    a, b = F.dump(inc), F.dump(fresh)
    shutil.rmtree(fresh)
    if a != b:
        bad += 1
        print(f"step {i}: inc_only {sorted((Counter(a)-Counter(b)).elements())} fresh_only {sorted((Counter(b)-Counter(a)).elements())}")
    else:
        print(f"step {i}: ok ({len(a)} edges)")
shutil.rmtree(base)
