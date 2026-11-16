import sqlite3, sys, collections
RANK = {'extracted': 2, 'inferred': 1, 'ambiguous': 0}
def load(db):
    c = sqlite3.connect(db)
    q = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name, sn.name)||':'||sn.type,
                  tf.path||':'||COALESCE(tn.qualified_name, tn.name)||':'||tn.type, e.confidence
           FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
           JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
    pairs = {}
    for rel, s, t, conf in c.execute(q):
        k = (rel, s, t)
        r = RANK.get(conf, -1)
        if k not in pairs or r > pairs[k]:
            pairs[k] = r
    return pairs
base, head = load(sys.argv[1]), load(sys.argv[2])
rels = sorted({k[0] for k in base} | {k[0] for k in head})
inv = {v: k for k, v in RANK.items()}
for rel in rels:
    b = {k: v for k, v in base.items() if k[0] == rel}
    h = {k: v for k, v in head.items() if k[0] == rel}
    rem = [k for k in b if k not in h]
    add = [k for k in h if k not in b]
    relab = [(k, b[k], h[k]) for k in b if k in h and b[k] != h[k]]
    rem_above = [k for k in rem if b[k] >= 1]
    addc = collections.Counter(inv[h[k]] for k in add)
    relc = collections.Counter(f"{inv[x]}->{inv[y]}" for _, x, y in relab)
    print(f"{rel}: {len(b)} -> {len(h)}  removed={len(rem)} (above floor {len(rem_above)})  added={len(add)} {dict(addc)}  relabelled={len(relab)} {dict(relc)}")
if len(sys.argv) > 3:
    rel = sys.argv[3]
    for k in sorted(k for k in base if k[0]==rel and k not in head): print("-", inv[base[k]], k[1], "->", k[2])
    for k in sorted(k for k in head if k[0]==rel and k not in base): print("+", inv[head[k]], k[1], "->", k[2])
    for k in sorted(k for k in base if k[0]==rel and k in head and base[k]!=head[k]): print("~", inv[base[k]], "->", inv[head[k]], k[1], "->", k[2])
