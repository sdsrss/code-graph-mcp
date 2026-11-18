#!/usr/bin/env python3
"""rows_cmp.py <base.jsonl> <head.jsonl>: compare two all-steps runs over the same seeds.

Reports (1) seeds/steps where head's edge sets without confidence differ from base's
(incremental or fresh), and (2) divergent rows head has at some step that base does not
have at the same step, with and without the confidence column.
"""
import json, sys
def load(f):
    return {r['seed']: r for r in map(json.loads, open(f))}
a, h = load(sys.argv[1]), load(sys.argv[2])
def rows(r):
    out = {}
    for d in r.get('diffs', []):
        out[d['step']] = {('I', x.strip()) for x in d['inc_only']} | {('F', x.strip()) for x in d['fresh_only']}
    return out
def noconf(rs):
    o = set()
    for side, x in rs:
        f = x.split(' | ')
        o.add((side, ' | '.join(f[:3] + f[4:])))
    return o
hash_diff, extra, extra_nc = [], [], []
nb = nh = 0
for s in sorted(h):
    ra, rh = a[s], h[s]
    if 'error' in ra or 'error' in rh:
        print('error', s); continue
    nb += not ra.get('ok'); nh += not rh.get('ok')
    if ra.get('hashes') != rh.get('hashes'):
        hash_diff.append(s)
    pa, ph = rows(ra), rows(rh)
    for step, rs in ph.items():
        new = rs - pa.get(step, set())
        if new:
            extra.append((s, step, sorted(new)))
        new_nc = noconf(rs) - noconf(pa.get(step, set()))
        if new_nc:
            extra_nc.append((s, step, sorted(new_nc)))
print(f"seeds {len(h)}: base diverged {nb}, head diverged {nh}")
print(f"edge sets without confidence differ (any step, inc or fresh): {len(hash_diff)} seeds {hash_diff[:20]}")
print(f"head-only divergent rows: {len({x[0] for x in extra})} seeds; ignoring confidence: {len({x[0] for x in extra_nc})} seeds")
for s, step, rs in extra[:12]:
    print(f"  seed {s} step {step}:")
    for side, x in rs[:4]:
        print(f"    {side} {x}")
