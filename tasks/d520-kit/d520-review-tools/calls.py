#!/usr/bin/env python3
"""calls.py base.edges head.edges: group calls edges by (src file, src name, src line, target name, normalized meta);
report groups whose target set changed: lost-all, gained-from-none, narrowed, replaced."""
import sys, json
from collections import defaultdict, Counter
def load(p):
    g = defaultdict(set)
    for line in open(p):
        f = line.rstrip("\n").split("\t")
        if len(f) < 9 or f[6] != "calls": continue
        meta = f[7]
        if meta.startswith("{"):
            m = json.loads(meta); m.pop("attr", None); meta = json.dumps(m, sort_keys=True)
        g[(f[0], f[1], f[2], f[4], meta)].add((f[3], f[5], f[8]))
    return g
a, b = load(sys.argv[1]), load(sys.argv[2])
kinds = Counter(); ex = defaultdict(list)
for k in set(a) | set(b):
    x, y = a.get(k, set()), b.get(k, set())
    if x == y: continue
    xs = {t[:2] for t in x}; ys = {t[:2] for t in y}
    if not y: kind = "lost-all"
    elif not x: kind = "new"
    elif ys < xs: kind = "narrowed"
    elif ys == xs: kind = "relabel"
    elif ys > xs: kind = "widened"
    else: kind = "replaced"
    kinds[kind] += 1; ex[kind].append((k, sorted(x), sorted(y)))
print(dict(kinds))
for kind in sys.argv[3:]:
    for k, x, y in sorted(ex[kind]):
        print(kind, k[0], k[1], k[2], "name=", k[3], k[4]); print("   base:", x); print("   head:", y)
