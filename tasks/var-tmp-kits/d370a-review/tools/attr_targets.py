#!/usr/bin/env python3
"""attr_targets.py <db>: classify targets of attr-bound (attr=1, no amb) Python call edges."""
import sqlite3, sys, collections
con = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
spans = collections.defaultdict(list)
for fid, nid, t, s, e in con.execute("select file_id, id, type, start_line, end_line from nodes where type in ('function','class','method')"):
    spans[fid].append((s, e, nid, t))
def enclosing(fid, nid, s, e):
    return [t for (s2, e2, n2, t) in spans[fid] if n2 != nid and s2 <= s and e <= e2 and (s2, e2) != (s, e)]
c = collections.Counter(); ex = collections.defaultdict(list)
for sp, sn, tp, tn, tt, fid, nid, s, e, conf in con.execute("""
  select sf.path, sn.name, tf.path, tn.name, tn.type, tn.file_id, tn.id, tn.start_line, tn.end_line, ed.confidence
  from edges ed join nodes sn on sn.id=ed.source_id join files sf on sf.id=sn.file_id
  join nodes tn on tn.id=ed.target_id join files tf on tf.id=tn.file_id
  where ed.relation='calls' and json_extract(ed.metadata,'$.attr')=1 and json_extract(ed.metadata,'$.amb') is null"""):
    enc = enclosing(fid, nid, s, e)
    kind = tt if not enc else f"{tt}-inside-{'function' if any(x in ('function','method') for x in enc) else 'class'}"
    c[(kind, conf)] += 1
    if len(ex[kind]) < 8: ex[kind].append(f"{sp}:{sn} -> {tp}:{tn}@{s}")
for k, v in sorted(c.items()): print(k, v)
for k, v in ex.items():
    if k not in ('function', 'class'):
        print("##", k); [print("   ", x) for x in v]
