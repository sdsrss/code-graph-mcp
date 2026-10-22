import sys, os, sqlite3
sys.path.insert(0, "/var/tmp/d175-review/harness")
from corp import *
lab, binary, src, nfiles = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
w = f"{R}/work/restore_{os.path.basename(os.path.dirname(src))}_{lab}"
inc, fr = w + "/inc", w + "/fresh"
copy(src, inc)
print("full", index(binary, inc)[:2])
con = sqlite3.connect(f"file:{inc}/.code-graph/index.db?mode=ro", uri=True)
targets = [r[0] for r in con.execute("""
  select tf.path, count(*) c from edges e join nodes s on s.id=e.source_id join nodes t on t.id=e.target_id
  join files tf on tf.id=t.file_id join files sf on sf.id=s.file_id
  where sf.language='python' and t.type='method' and sf.id<>tf.id
    and exists(select 1 from nodes o where o.file_id=t.file_id and o.name=t.name and o.type<>'method')
  group by tf.path order by c desc""")]
con.close()
print("candidate target files", len(targets), targets[:nfiles])
for t in targets[:nfiles]:
    open(os.path.join(inc, t), "a").write("\n# review edit\n")
print("incr", index(binary, inc)[:2])
copy(inc, fr)
print("fresh", index(binary, fr)[:2])
oa, of = diff(dump(inc), dump(fr), 15, lab)
import collections
print("   only-fresh by (relation, confidence, target type):", collections.Counter((k[8], k[10], k[7]) for k in of.elements()))
print("   only-inc by (relation, confidence, target type):", collections.Counter((k[8], k[10], k[7]) for k in oa.elements()))
