"""pairs.py A.db B.db [relation] [--list]: distinct (relation, caller, callee) pairs keyed by file:qualified_name; confidence = max over duplicates."""
import sqlite3, sys, collections
RANK = {'extracted': 2, 'inferred': 1, 'ambiguous': 0}
INV = {v: k for k, v in RANK.items()}
def load(db, typed=False):
    c = sqlite3.connect(db)
    q = f"""SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name){"||':'||sn.type" if typed else ""},
                  tf.path||':'||COALESCE(tn.qualified_name, tn.name){"||':'||tn.type" if typed else ""}, e.confidence
           FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
           JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
    pairs = {}
    for rel, s, t, conf in c.execute(q):
        k = (rel, s, t); r = RANK.get(conf, -1)
        if k not in pairs or r > pairs[k]: pairs[k] = r
    return pairs
args = [a for a in sys.argv[1:] if not a.startswith('--')]
typed = '--typed' in sys.argv
a, b = load(args[0], typed), load(args[1], typed)
only = args[2] if len(args) > 2 else None
for rel in sorted({k[0] for k in a} | {k[0] for k in b}):
    if only and rel != only: continue
    A = {k: v for k, v in a.items() if k[0] == rel}; B = {k: v for k, v in b.items() if k[0] == rel}
    rem = [k for k in A if k not in B]; add = [k for k in B if k not in A]
    relab = [(k, A[k], B[k]) for k in A if k in B and A[k] != B[k]]
    print(f"{rel}: {len(A)} -> {len(B)} removed={len(rem)} (above floor {sum(A[k]>=1 for k in rem)}) added={len(add)} {dict(collections.Counter(INV[B[k]] for k in add))} relabelled={len(relab)} {dict(collections.Counter(f'{INV[x]}->{INV[y]}' for _,x,y in relab))}")
    if '--list' in sys.argv:
        for k in sorted(rem): print('  -', INV[A[k]], k[1], '->', k[2])
        for k in sorted(add): print('  +', INV[B[k]], k[1], '->', k[2])
        for k, x, y in sorted(relab): print('  ~', INV[x], '->', INV[y], k[1], '->', k[2])
