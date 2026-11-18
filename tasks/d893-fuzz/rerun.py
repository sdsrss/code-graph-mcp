"""rerun.py <jsonl> <binary>: replay every seed in the file; print divergence rows."""
import json, sys, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import fuzz_seq as F, random
binary = os.path.abspath(sys.argv[2])
for r in map(json.loads, open(sys.argv[1])):
    states = r['states']
    # replay the stored states instead of regenerating from the seed
    def run():
        import shutil, tempfile, subprocess
        base = tempfile.mkdtemp(prefix=f"v{r['seed']}-", dir=F.ROOT)
        try:
            inc = base + "/inc"; os.makedirs(inc); subprocess.run(["git", "init", "-q"], cwd=inc, check=True)
            stamp, prev = 1_700_000_000, None
            for i, st in enumerate(states):
                stamp += 10; F.write_state(inc, st, prev, stamp); prev = st; F.index(binary, inc)
                if i == 0: continue
                fr = f"{base}/fresh{i}"; os.makedirs(fr); subprocess.run(["git", "init", "-q"], cwd=fr, check=True)
                F.write_state(fr, st, None, stamp); F.index(binary, fr)
                a, b = F.dump(inc), F.dump(fr)
                if a != b:
                    return i, sorted(set(a) - set(b)), sorted(set(b) - set(a))
            return None
        finally:
            shutil.rmtree(base, ignore_errors=True)
    res = run()
    if res is None: print(f"seed {r['seed']}: OK")
    else:
        i, ai, bf = res; print(f"seed {r['seed']}: DIVERGED at step {i}")
        for x in ai: print("   INC  ", x)
        for x in bf: print("   FRESH", x)
