import sqlite3, sys, collections, json
# edgediff.py BASE_DB HEAD_DB: full edge diff (all relations), keyed by endpoints' (path, qname, line)
def load(p):
    db = sqlite3.connect(f"file:{p}?mode=ro", uri=True)
    q = """SELECT sf.path, coalesce(s.qualified_name, s.name), s.start_line, e.relation,
                  tf.path, coalesce(t.qualified_name, t.name), t.start_line, e.confidence, coalesce(e.metadata,'')
           FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id
           JOIN nodes t ON t.id=e.target_id JOIN files tf ON tf.id=t.file_id"""
    out = collections.Counter()
    rows = 0
    for r in db.execute(q):
        out[r[:8]] = 1; rows += 1
    print(p.split('/arms/')[-1], 'rows', rows, 'distinct', len(out))
    meta = {}
    for r in db.execute(q):
        meta.setdefault(r[:8], set()).add(r[8])
    return out, meta
b, bm = load(sys.argv[1]); h, hm = load(sys.argv[2])
gone = b - h; new = h - b
# pair removed/added by (src, rel, tgt) to find tier moves
key = lambda k: k[:7]
gk = {key(k): k for k in gone}; nk = {key(k): k for k in new}
moved = [(gk[x], nk[x]) for x in gk.keys() & nk.keys()]
only_gone = [k for k in gone if key(k) not in nk]; only_new = [k for k in new if key(k) not in gk]
print(f"edges base {sum(b.values())} head {sum(h.values())}; removed {len(only_gone)} added {len(only_new)} relabelled {len(moved)}")
c = collections.Counter((k[3], k[7], "same" if k[0]==k[4] else "cross") for k in only_gone)
print("removed by (relation, tier, same/cross):", dict(c))
c = collections.Counter((k[3], k[7], "same" if k[0]==k[4] else "cross") for k in only_new)
print("added by (relation, tier, same/cross):", dict(c))
c = collections.Counter((a[7], z[7]) for a, z in moved); print("relabelled:", dict(c))
if len(sys.argv) > 3:
    for k in sorted(only_new)[:int(sys.argv[3])]: print("  +", k, hm.get(k))
    for a, z in sorted(moved)[:int(sys.argv[3])]: print("  ~", a, "->", z[7])
