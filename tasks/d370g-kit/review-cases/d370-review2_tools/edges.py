import sqlite3, sys
d = sys.argv[1]; filt = sys.argv[2] if len(sys.argv) > 2 else None
con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
Q = """select sf.path, sn.name, sn.start_line, tf.path, coalesce(tn.qualified_name, tn.name), tn.start_line, coalesce(e.metadata,''), e.confidence
from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id
where e.relation='calls' order by 1,3,2,4,5"""
for r in con.execute(Q):
    s = f"{r[0]}:{r[1]}@{r[2]} -> {r[3]}:{r[4]}@{r[5]} {r[6]} [{r[7]}]"
    if filt is None or filt in s: print(s)
for r in con.execute("select sf.path, sn.name, sn.start_line, p.target_name, coalesce(p.metadata,'') from pending_unresolved_calls p join nodes sn on sn.id=p.source_id join files sf on sf.id=sn.file_id"):
    print("PENDING", r)
