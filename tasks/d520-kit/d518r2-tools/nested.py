#!/usr/bin/env python3
"""nested.py <corpus>: call edges into a definition nested in another definition
(its line range strictly inside a function/method/class of the same file), new in
head vs pre (by source node, target node), with confidence; and every head edge
into such a target that carries `"attr":` metadata."""
import sqlite3, sys, collections
c = sys.argv[1]
R = "/var/tmp/d518r2/idx"
Q = """
with defs as (select n.id, n.file_id, n.name, n.type, n.start_line, n.end_line, n.qualified_name from nodes n
              where n.type in ('function','method','class')),
     nested as (select d.id from defs d where exists (
          select 1 from defs o where o.file_id = d.file_id and o.id <> d.id
            and o.start_line <= d.start_line and o.end_line >= d.end_line
            and (o.start_line < d.start_line or o.end_line > d.end_line)))
select sf.path, sn.name, sn.start_line, tf.path, tn.name, tn.type, tn.start_line, coalesce(tn.qualified_name,''),
       e.relation, coalesce(e.metadata,''), e.confidence
from edges e join nodes sn on sn.id = e.source_id join files sf on sf.id = sn.file_id
             join nodes tn on tn.id = e.target_id join files tf on tf.id = tn.file_id
where e.relation = 'calls' and e.target_id in (select id from nested) and sf.id <> tf.id
"""
def load(arm):
    con = sqlite3.connect(f"file:{R}/{c}-{arm}/.code-graph/index.db?mode=ro", uri=True)
    rows = con.execute(Q).fetchall(); con.close(); return rows
pre, head = load("pre"), load("head")
k = lambda r: r[:8]
pk = collections.Counter(k(r) for r in pre)
new = [r for r in head if pk[k(r)] == 0]
print(c, "cross-file calls into nested defs: pre", len(pre), "head", len(head), "new in head (by source,target):", len(new))
for r in new[:20]: print("   NEW", r)
attr = [r for r in head if '"attr":' in r[9]]
print(c, "head attr-call edges into nested defs:", len(attr), collections.Counter((r[3], r[4], r[10]) for r in attr).most_common(10))
pre_conf = collections.Counter((r[3], r[4], r[10]) for r in pre if r[4] in ("fsencode", "fsdecode"))
head_conf = collections.Counter((r[3], r[4], r[10]) for r in head if r[4] in ("fsencode", "fsdecode"))
if pre_conf or head_conf: print(c, "fsencode/fsdecode pre:", dict(pre_conf), "head:", dict(head_conf))
