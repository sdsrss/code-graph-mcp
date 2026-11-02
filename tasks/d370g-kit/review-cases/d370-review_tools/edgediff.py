#!/usr/bin/env python3
"""edgediff.py A_DIR B_DIR: compare every edge (endpoint files+qnames+lines, relation,
metadata, confidence), then endpoint pairs per relation, then (pair, confidence)."""
import sqlite3, sys, collections
Q = """select coalesce(sf.path,'?'), coalesce(s.qualified_name,s.name), s.start_line, coalesce(tf.path,'?'), coalesce(t.qualified_name,t.name), t.start_line,
       e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes s on s.id=e.source_id left join files sf on sf.id=s.file_id
       join nodes t on t.id=e.target_id left join files tf on tf.id=t.file_id"""
def load(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    r = [tuple(x) for x in con.execute(Q)]; con.close(); return r
A, B = load(sys.argv[1]), load(sys.argv[2])
ca, cb = collections.Counter(A), collections.Counter(B)
print(f"rows A={len(A)} B={len(B)} full-row diff: A-only={sum((ca-cb).values())} B-only={sum((cb-ca).values())}")
pair = lambda r: (r[0], r[1], r[2], r[3], r[4], r[5], r[6])
pa, pb = {pair(r) for r in A}, {pair(r) for r in B}
print(f"distinct endpoint pairs A={len(pa)} B={len(pb)} lost={len(pa-pb)} gained={len(pb-pa)}")
pc = lambda r: pair(r) + (r[8],)
qa, qb = {pc(r) for r in A}, {pc(r) for r in B}
print(f"(pair,confidence) lost={len(qa-qb)} gained={len(qb-qa)}")
# best tier per pair
rank = {'extracted': 0, 'inferred': 1, 'ambiguous': 2}
def best(rows):
    d = {}
    for r in rows:
        k = pair(r); t = r[8]
        if k not in d or rank.get(t, 9) < rank.get(d[k], 9): d[k] = t
    return d
ba, bb = best(A), best(B)
moves = collections.Counter((ba[k], bb[k]) for k in ba.keys() & bb.keys() if ba[k] != bb[k])
print("best-tier moves:", dict(moves))
dflt = lambda d: {k for k, t in d.items() if t in ('extracted', 'inferred')}
da, dbb = dflt(ba), dflt(bb)
print(f"default-floor pairs A={len(da)} B={len(dbb)} lost={len(da-dbb)} gained={len(dbb-da)}")
N = int(sys.argv[3]) if len(sys.argv) > 3 else 15
for k in sorted(dbb - da)[:N]: print("  GAINED-DEFAULT", k, bb[k])
for k in sorted(da - dbb)[:N]: print("  LOST-DEFAULT  ", k, ba[k], "->", bb.get(k))
full_only_a = ca - cb; full_only_b = cb - ca
if len(sys.argv) > 4:
    for r in sorted(full_only_a)[:N]: print("  ROW A-only", r)
    for r in sorted(full_only_b)[:N]: print("  ROW B-only", r)
