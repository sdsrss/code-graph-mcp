#!/usr/bin/env python3
"""c4check.py <base_dir> <head_dir>: for call sites whose only q=module edges in HEAD carry no attr, compare
BASE vs HEAD q=module target sets (paths+lines) and confidences."""
import sqlite3, sys, collections
Q = """select sf.path, sn.name, sn.start_line, tn.name, tf.path||'@'||tn.start_line, coalesce(e.metadata,''), e.confidence
 from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
 where e.relation='calls' and json_extract(e.metadata,'$.q')='module'"""
def load(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    g = collections.defaultdict(set); attr = set()
    for r in con.execute(Q):
        k = r[:4]; g[k].add((r[4], r[6]))
        if '"attr"' in r[5]: attr.add(k)
    return g, attr
gb, _ = load(sys.argv[1]); gh, ah = load(sys.argv[2])
keys = [k for k in set(gb) | set(gh) if k not in ah]
diff = [k for k in keys if gb.get(k) != gh.get(k)]
print(f"non-attr q=module call sites: {len(keys)}; differing BASE vs HEAD: {len(diff)}")
for k in sorted(diff)[:15]: print("  ", k, "\n     BASE", sorted(gb.get(k, ())), "\n     HEAD", sorted(gh.get(k, ())))
