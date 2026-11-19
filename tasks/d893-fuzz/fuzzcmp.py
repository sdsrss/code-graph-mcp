import json, sys, collections
def load(f): return {r['seed']: r for r in map(json.loads, open(f))}
a, b = load(sys.argv[1]), load(sys.argv[2])
def rows(r, kind):  # all steps' diff rows
    return sum((len(d['inc_only']) + len(d['fresh_only']) for d in r.get('diffs', [])), 0) if kind == 'all' else (len(r.get('inc_only', [])) + len(r.get('fresh_only', [])))
bad = lambda r: not r.get('ok')
for name, x in (("A", a), ("B", b)):
    bs = [s for s in x if bad(x[s])]
    print(name, "diverged", len(bs), "errors", sum('error' in x[s] for s in x), "rows(all steps)", sum(rows(x[s], 'all') for s in bs), "rows(first step)", sum(rows(x[s], 'first') for s in bs))
only_b = sorted(s for s in b if bad(b[s]) and not bad(a[s]))
print("B-only seeds", len(only_b), only_b[:20])
# per seed per step: B's rows subset of A's rows at the same step?
viol = []
for s in b:
    if not bad(b[s]): continue
    da = {d['step']: d for d in a[s].get('diffs', [])}
    for d in b[s].get('diffs', []):
        A = da.get(d['step'], {'inc_only': [], 'fresh_only': []})
        for k in ('inc_only', 'fresh_only'):
            extra = collections.Counter(d[k]) - collections.Counter(A[k])
            if extra: viol.append((s, d['step'], k, list(extra.elements())))
print("B rows not in A at same step:", len(viol))
for v in viol[:10]: print("  ", v)
