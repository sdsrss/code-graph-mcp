import sqlite3, sys
# dump.py DB : call edges + pending rows, one per line, sorted
db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
q = """SELECT sf.path, coalesce(s.qualified_name, s.name), s.start_line,
              tf.path, coalesce(t.qualified_name, t.name), t.start_line, e.confidence, coalesce(e.metadata,'')
       FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id
       JOIN nodes t ON t.id=e.target_id JOIN files tf ON tf.id=t.file_id WHERE e.relation='calls'"""
rows = [f"E {r[0]}:{r[1]}@{r[2]} -> {r[3]}:{r[4]}@{r[5]} [{r[6]}] {r[7]}" for r in db.execute(q)]
try:
    q2 = """SELECT sf.path, coalesce(s.qualified_name, s.name), s.start_line, p.target_name, coalesce(p.metadata,'')
            FROM pending_unresolved_calls p JOIN nodes s ON s.id=p.source_id JOIN files sf ON sf.id=s.file_id"""
    rows += [f"P {r[0]}:{r[1]}@{r[2]} -> {r[3]} {r[4]}" for r in db.execute(q2)]
except Exception as e:
    rows.append(f"! pending: {e}")
for r in sorted(rows): print(r)
