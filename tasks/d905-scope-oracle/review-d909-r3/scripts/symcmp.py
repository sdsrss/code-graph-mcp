import json, sys, collections
def load(f): return {r['seed']: r for r in map(json.loads, open(f))}
a, b = load(sys.argv[1]), load(sys.argv[2])
assert set(a) == set(b)
def steps(r): return {d['step']: (collections.Counter(d['inc_only']), collections.Counter(d['fresh_only'])) for d in r.get('diffs', [])}
same = diff = 0; ex = []
for s in a:
    if steps(a[s]) == steps(b[s]): same += 1
    else:
        diff += 1; ex.append(s)
print(f"seeds {len(a)}: A diverged {sum(not r.get('ok') for r in a.values())} B diverged {sum(not r.get('ok') for r in b.values())}; errors A {sum('error' in r for r in a.values())} B {sum('error' in r for r in b.values())}; seeds with identical per-step diff rows {same}, differing {diff} {ex[:10]}")
