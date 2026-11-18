"""repro.py <jsonl> <seed> <binary> [grep]: replay the seed's states, print edges matching grep (inc vs fresh) at the last state."""
import json, sys, os, shutil, subprocess
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fuzz_seq as F
f, seed, binary = sys.argv[1], int(sys.argv[2]), os.path.abspath(sys.argv[3])
pat = sys.argv[4] if len(sys.argv) > 4 else ''
r = next(r for r in map(json.loads, open(f)) if r['seed'] == seed)
base = os.path.join(F.ROOT, f"repro-{seed}")
shutil.rmtree(base, ignore_errors=True)
inc = base + "/inc"; os.makedirs(inc); subprocess.run(["git", "init", "-q"], cwd=inc)
stamp, prev = 1_700_000_000, None
for st in r['states']:
    stamp += 10; F.write_state(inc, st, prev, stamp); prev = st; F.index(binary, inc)
fresh = base + "/fresh"; os.makedirs(fresh); subprocess.run(["git", "init", "-q"], cwd=fresh)
F.write_state(fresh, r['states'][-1], None, stamp); F.index(binary, fresh)
a, b = F.dump(inc), F.dump(fresh)
for name, rows in (("INC", a), ("FRESH", b)):
    print("==", name)
    for x in rows:
        if pat in x: print("  ", x)
