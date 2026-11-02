# edgediff.py DB_A DB_B : compare every edge (paths, names, lines, relation, metadata, confidence) and (caller,callee) pairs
import sqlite3, sys
from collections import Counter
Q = """select coalesce(sf.path,'?'), sn.qualified_name, sn.name, sn.start_line, coalesce(tf.path,'?'), tn.qualified_name, tn.name, tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def load(p):
    con = sqlite3.connect(f"file:{p}?mode=ro", uri=True); r = Counter(tuple(x) for x in con.execute(Q)); con.close(); return r
a, b = load(sys.argv[1]), load(sys.argv[2])
oa, ob = a - b, b - a
pairs = lambda c: Counter((r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8]) for r in c.elements())
pa, pb = pairs(a), pairs(b)
print(f"edges A={sum(a.values())} B={sum(b.values())} only-A={sum(oa.values())} only-B={sum(ob.values())} | pairs only-A={sum((pa-pb).values())} only-B={sum((pb-pa).values())}")
lim = int(sys.argv[3]) if len(sys.argv) > 3 else 10
for r in list(oa.elements())[:lim]: print("  A-only", r)
for r in list(ob.elements())[:lim]: print("  B-only", r)
