import sqlite3, sys
db = sys.argv[1]; filt = sys.argv[2] if len(sys.argv) > 2 else None
con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
Q = """select sf.path, sn.name, sn.start_line, coalesce(tf.path,'?'), tn.qualified_name, tn.name, tn.start_line, e.relation, coalesce(e.metadata,''), e.confidence
 from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
 join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id where e.relation='calls' order by 1,3,2,7"""
for r in con.execute(Q):
    s = f"{r[0]}:{r[1]}@{r[2]} -> {r[3]}:{r[4] or r[5]}@{r[6]} [{r[9]}] {r[8]}"
    if filt and filt not in s: continue
    print("  ", s)
