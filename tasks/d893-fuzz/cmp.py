import json, sys, collections
def load(f):
    return {r['seed']: r for r in map(json.loads, open(f))}
a, b = load(sys.argv[1]), load(sys.argv[2])
bad = lambda r: not r.get('ok')
only_a = sorted(s for s in a if bad(a[s]) and not bad(b[s]))
only_b = sorted(s for s in b if bad(b[s]) and not bad(a[s]))
both = sorted(s for s in a if bad(a[s]) and bad(b[s]))
print(f"diverged: {sys.argv[1]} only {len(only_a)}, {sys.argv[2]} only {len(only_b)}, both {len(both)}")
print("only second:", only_b[:40])
print("only first:", only_a[:40])
