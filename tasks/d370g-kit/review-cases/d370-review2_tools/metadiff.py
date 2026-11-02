import sqlite3, sys, collections
def load(p):
    db = sqlite3.connect(f"file:{p}/.code-graph/index.db?mode=ro", uri=True)
    q = """SELECT sf.path, coalesce(s.qualified_name, s.name), s.start_line, e.relation,
                  tf.path, coalesce(t.qualified_name, t.name), t.start_line, e.confidence, coalesce(e.metadata,'')
           FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id
           JOIN nodes t ON t.id=e.target_id JOIN files tf ON tf.id=t.file_id"""
    e = collections.Counter(db.execute(q))
    p = collections.Counter(db.execute("""select sf.path, sn.name, sn.start_line, p.target_name, coalesce(p.metadata,'') from pending_unresolved_calls p
        join nodes sn on sn.id=p.source_id join files sf on sf.id=sn.file_id"""))
    own = sum(v for k, v in e.items() if '"own"' in k[8])
    return e, p, own
b, bp, bo = load(sys.argv[1]); h, hp, ho = load(sys.argv[2])
print(f"rows base {sum(b.values())} head {sum(h.values())}; own-marked head edges {ho}; rows only-base {sum((b-h).values())} only-head {sum((h-b).values())}")
nb = collections.Counter(k[8] for k in (b-h).elements()); nh = collections.Counter(k[8][:40] for k in (h-b).elements())
print(" only-base meta:", nb.most_common(4)); print(" only-head meta:", nh.most_common(4))
print(f" pending base {sum(bp.values())} head {sum(hp.values())}; only-base {sum((bp-hp).values())} only-head {sum((hp-bp).values())}")
for r in list((hp-bp).elements())[:5]: print("  P+", r)
for r in list((bp-hp).elements())[:5]: print("  P-", r)
