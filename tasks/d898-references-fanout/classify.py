#!/usr/bin/env python3
"""classify.py <replay-work-dir>: why is each fresh-only edge of a replay missing?

For every edge only the fresh index has, looks at the index the incremental run
started from (old.db) and the fresh one (fresh.db) and reports, per relation:
  - whether the source file changed in the run (hash differs or file is new);
  - how many definitions of the target's name in the source file's language
    the old index had, outside the source file (0 = the run made the name exist);
  - whether the old index had an edge of the same relation from the same source
    node to a node of that name, and its confidence.
"""
import json, os, sqlite3, sys
from collections import Counter

work = sys.argv[1]
d = json.load(open(os.path.join(work, "diff.json")))
old = sqlite3.connect(f"file:{work}/old.db?mode=ro", uri=True)
fresh = sqlite3.connect(f"file:{work}/fresh.db?mode=ro", uri=True)

def ends(row):
    rel, src, tgt, conf, meta = row.split(" | ")
    sp, sq, st = src.rsplit(":", 2)
    tp, tq, tt = tgt.rsplit(":", 2)
    return rel, sp, sq, st, tp, tq, tt, conf, meta

def node_name(db, path, qn, ty):
    r = db.execute("SELECT n.name FROM nodes n JOIN files f ON f.id=n.file_id WHERE f.path=? AND "
                   "COALESCE(n.qualified_name,n.name)=? AND n.type=? LIMIT 1", (path, qn, ty)).fetchone()
    return r[0] if r else None

def file_row(db, path):
    return db.execute("SELECT blake3_hash, language FROM files WHERE path=?", (path,)).fetchone()

out = Counter()
examples = {}
for row in d["fresh_only"]:
    rel, sp, sq, st, tp, tq, tt, conf, meta = ends(row)
    name = node_name(fresh, tp, tq, tt)
    fo, ff = file_row(old, sp), file_row(fresh, sp)
    changed = "new" if fo is None else ("changed" if fo[0] != ff[0] else "unchanged")
    lang = ff[1]
    ndefs = old.execute("SELECT COUNT(*) FROM nodes n JOIN files f ON f.id=n.file_id WHERE n.name=? "
                        "AND f.language IS ? AND f.path<>? AND f.path<>'<external>'",
                        (name, lang, sp)).fetchone()[0]
    prev = old.execute(
        "SELECT e.confidence FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id "
        "JOIN nodes t ON t.id=e.target_id WHERE sf.path=? AND COALESCE(s.qualified_name,s.name)=? "
        "AND s.type=? AND t.name=? AND e.relation=?", (sp, sq, st, name, rel)).fetchall()
    tgt_new = file_row(old, tp) is None
    key = (rel, conf, changed, "old-defs=0" if ndefs == 0 else "old-defs>0",
           "old-edge:" + ",".join(sorted({p[0] for p in prev})) if prev else "old-edge:none",
           "target-file-new" if tgt_new else "target-file-old")
    out[key] += 1
    examples.setdefault(key, (row, name, ndefs))
for k, n in out.most_common():
    print(n, k)
    print("   e.g.", examples[k][0][:200], "| name", examples[k][1], "| old defs", examples[k][2])
