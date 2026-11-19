import sqlite3, sys
from collections import Counter
R = "/var/tmp/d905-review/keep"
Q = """SELECT e.relation, sf.path, sn.type, COALESCE(sn.qualified_name,sn.name), sn.start_line, sn.end_line,
 tf.path, tn.type, COALESCE(tn.qualified_name,tn.name), tn.start_line, tn.end_line, e.confidence, COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
NQ = "SELECT f.path, n.type, COALESCE(n.qualified_name,n.name), n.start_line, n.end_line, n.signature FROM nodes n JOIN files f ON f.id=n.file_id"
for n in sys.argv[1:]:
    res = {}
    for arm in ("base", "head"):
        con = sqlite3.connect(f"file:{R}/{n}-{arm}/.code-graph/index.db?mode=ro", uri=True)
        res[arm] = (Counter(con.execute(Q)), Counter(con.execute(NQ)), con.execute("select count(*) from pending_unresolved_calls").fetchone()[0] if con.execute("select 1 from sqlite_master where name='pending_unresolved_calls'").fetchone() else None)
        con.close()
    (eb, nb, pb), (eh, nh, ph) = res["base"], res["head"]
    print(n, "edges", sum(eb.values()), "->", sum(eh.values()), "base-only", sum((eb-eh).values()), "head-only", sum((eh-eb).values()),
          "| nodes same:", nb == nh, sum(nb.values()), sum(nh.values()), "| pending rows", pb, "->", ph)
    for r in list((eh-eb).elements())[:5]: print("  HEAD-ONLY", r)
