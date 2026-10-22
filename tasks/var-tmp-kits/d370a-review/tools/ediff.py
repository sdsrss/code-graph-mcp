#!/usr/bin/env python3
"""ediff.py <dirA> <dirB> [--show N] [--filter substr]: compare edges of two indexes ignoring metadata
(and separately with metadata). Groups by relation."""
import sqlite3, sys, collections
A, B = sys.argv[1], sys.argv[2]
show = int(sys.argv[sys.argv.index('--show')+1]) if '--show' in sys.argv else 10
flt = sys.argv[sys.argv.index('--filter')+1] if '--filter' in sys.argv else None
Q = """select coalesce(sf.path,'?'), sn.name, sn.start_line, coalesce(tf.path,'?'), tn.name, tn.start_line, tn.type,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def load(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = [tuple(r) for r in con.execute(Q)]; con.close(); return rows
ra, rb = load(A), load(B)
key = lambda r: r[:8]
ka = collections.Counter(key(r) for r in ra); kb = collections.Counter(key(r) for r in rb)
oa, ob = ka - kb, kb - ka
print(f"edges A={len(ra)} B={len(rb)}; ignoring metadata+confidence: only-A={sum(oa.values())} only-B={sum(ob.values())}")
byrel = collections.Counter((k[7], 'A') for k in oa.elements()) + collections.Counter((k[7], 'B') for k in ob.elements())
print("  by relation:", dict(byrel))
# label moves for edges whose (key without meta) exists in both
ca = collections.defaultdict(list); cb = collections.defaultdict(list)
for r in ra: ca[key(r)].append(r[9])
for r in rb: cb[key(r)].append(r[9])
moves = collections.Counter()
for k in ca:
    if k in cb and sorted(ca[k]) != sorted(cb[k]):
        for x, y in zip(sorted(ca[k]), sorted(cb[k])):
            if x != y: moves[(x, y, k[7])] += 1
print("  label moves:", dict(moves))
fm = lambda k: f"{k[0]}:{k[1]}@{k[2]} -{k[7]}-> {k[3]}:{k[4]}@{k[5]}({k[6]})"
for tag, o in (("-A", oa), ("+B", ob)):
    n = 0
    for k in sorted(o):
        if flt and flt not in fm(k): continue
        if n < show: print("  ", tag, fm(k))
        n += 1
