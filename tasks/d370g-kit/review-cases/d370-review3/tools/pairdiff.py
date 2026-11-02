import sqlite3, sys
from collections import Counter
Q = """select coalesce(sf.path,'?'), sn.qualified_name, sn.start_line, coalesce(tf.path,'?'), tn.qualified_name, tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def load(p):
    con = sqlite3.connect(f"file:{p}?mode=ro", uri=True); r = [tuple(x) for x in con.execute(Q)]; con.close(); return r
a, b = load(sys.argv[1]), load(sys.argv[2])
key = lambda r: r[:7]
ka, kb = Counter(map(key, a)), Counter(map(key, b))
sa, sb = set(ka), set(kb)
print("distinct pairs only-A", len(sa - sb), "only-B", len(sb - sa), "| multiplicity-only diffs", sum(((kb - ka)).values()) - len(sb - sa), sum(((ka - kb)).values()) - len(sa - sb))
for k in sorted(sb - sa): print("  new", k)
dups = [k for k in kb if kb[k] > ka.get(k, 0) and k in sa]
for k in dups[:10]:
    print("  more copies in B:", k, ka[k], "->", kb[k]); [print("     B", r[7:]) for r in b if key(r) == k]; [print("     A", r[7:]) for r in a if key(r) == k]
# by confidence/relation of removed
from collections import defaultdict
gone = sa - sb
lost = Counter((r[6], r[8], r[0] == r[3]) for r in a if key(r) in gone)
print("removed distinct pairs by (relation, confidence, same-file):", dict(lost))
