#!/usr/bin/env python3
"""Compare per-row differences between arms in a sweep JSON.
usage: cmp.py SWEEPJSON armA armB  -> rows armA has wrong that armB does not"""
import sys, json, re, collections
res = json.load(open(sys.argv[1]))
A, B = sys.argv[2], sys.argv[3]
def rows(out):
    s = set()
    for st in out:
        for r in st["extra"]:
            s.add((st["step"], "+", re.sub(r"@\d+\)", ")", r)))
        for r in st["missing"]:
            s.add((st["step"], "-", re.sub(r"@\d+\)", ")", r)))
    return s
tot = collections.Counter()
for name, r in sorted(res.items()):
    if A not in r or B not in r: continue
    only = rows(r[A]) - rows(r[B])
    if only:
        print(f"{name}: {A}-only {len(only)}")
        for x in sorted(only):
            print("   ", x)
        tot[name] += len(only)
print("cases:", len(tot), "rows:", sum(tot.values()))
