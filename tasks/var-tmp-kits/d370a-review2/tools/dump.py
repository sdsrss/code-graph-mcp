#!/usr/bin/env python3
"""dump.py <root> [--meta] [--py]: print every edge of <root>/.code-graph/index.db, one per line, sorted."""
import sqlite3, sys
root = sys.argv[1]
Q = """select coalesce(sf.path,'?'), sn.name, sn.start_line, sn.type, coalesce(tf.path,'?'), tn.name, tn.start_line, tn.type,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
rows = sorted(set(con.execute(Q)))
for r in rows:
    if '--py' in sys.argv and not r[0].endswith('.py'): continue
    print(f"{r[0]}:{r[1]}@{r[2]} -{r[8]}[{r[10]}]{r[9]}-> {r[4]}:{r[5]}@{r[6]}({r[7]})")
if '--pending' in sys.argv:
    for r in con.execute("select sf.path, sn.name, p.target_name, coalesce(p.metadata,'') from pending_unresolved_calls p join nodes sn on sn.id=p.source_id join files sf on sf.id=sn.file_id order by 1,2,3"):
        print("PENDING", *r)
