#!/usr/bin/env python3
"""flaskdiff.py <dbA> <dbB>: every edge only in A or only in B, keyed with
metadata; then the same diff with metadata dropped, so metadata-only changes
are told apart from target moves. Prints grouped by (relation, target)."""
import collections, sqlite3, sys
Q = """select sf.path, sn.name, tf.path, tn.name, tn.type, e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id
       join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id"""
def load(p):
    con = sqlite3.connect(f"file:{p}?mode=ro", uri=True); r = con.execute(Q).fetchall(); con.close(); return r
A, B = load(sys.argv[1]), load(sys.argv[2])
ka = collections.Counter(r[:7] for r in A); kb = collections.Counter(r[:7] for r in B)
na = collections.Counter(r[:6] for r in A); nb = collections.Counter(r[:6] for r in B)
print("with metadata: only-A", sum((ka-kb).values()), "only-B", sum((kb-ka).values()))
print("without metadata: only-A", sum((na-nb).values()), "only-B", sum((nb-na).values()))
meta_only = [k for k in (ka-kb) if k[:6] in nb and k[:6] in na and (na[k[:6]] == nb[k[:6]])]
print("metadata-only changes:", len(meta_only))
for k in sorted(meta_only)[:20]:
    newm = [x[6] for x in kb if x[:6] == k[:6]]
    print("   ", k[0], k[1], "->", k[2], k[3], k[5], k[6], "=>", newm)
for lbl, d in (("-A", na-nb), ("+B", nb-na)):
    g = collections.Counter((k[5], k[2] + ":" + k[3]) for k in d.elements())
    print(lbl, sum(d.values()))
    for (rel, tgt), n in sorted(g.items(), key=lambda x: (-x[1], x[0])): print(f"   {n:3d} {rel:8s} {tgt}")
