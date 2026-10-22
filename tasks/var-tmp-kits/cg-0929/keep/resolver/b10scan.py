#!/usr/bin/env python3
"""B10 census: JS/TS call edges whose every call site of the callee name in the
caller is `this.NAME(` (or `self.NAME(` / `that.NAME(`), with the verdict when the
oracle judged the pair.  b10scan.py ARM CORPUS"""
import collections, json, re, sqlite3, sys

arm, corpus = sys.argv[1], sys.argv[2]
root = f"{arm}/{corpus}"
db = sqlite3.connect(f"{root}/.code-graph/index.db")
judged = {}
try:
    for j in json.load(open(f"{arm}/out/{corpus}-javascript-judged.json")):
        judged[(j["caller_at"], j["caller"], j["callee_at"], j["callee"])] = j["verdict"]
except FileNotFoundError:
    pass
cache = {}
def lines_of(p):
    if p not in cache:
        try: cache[p] = open(f"{root}/{p}", encoding="utf-8", errors="replace").read().split("\n")
        except Exception: cache[p] = []
    return cache[p]
rows = collections.Counter(); ex = []
q = """SELECT sf.path, sn.name, sn.start_line, sn.end_line, tf.path, tn.name, tn.type, tn.start_line, e.confidence, e.metadata, sf.id=tf.id
       FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
       JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id
       WHERE e.relation='calls' AND sf.language IN ('javascript','typescript','tsx')"""
for sp, sn, sl, el, tp, tn, tt, tl, conf, meta, same in db.execute(q):
    L = lines_of(sp); short = tn.split(".")[-1]
    body = "\n".join(L[sl - 1:el])
    sites = [m.group(1) for m in re.finditer(r"(?:([\w$]+)\s*\.\s*)?(?<![\w$])" + re.escape(short) + r"\s*\(", body)]
    if not sites or any(s not in ("this", "self", "that", "_this") for s in sites):
        continue
    if meta and ('"rtype"' in meta or '"self"' in meta):
        continue  # a typed `this`
    v = judged.get((f"{sp}:{sl}", sn, f"{tp}:{tl}", tn), "unjudged")
    k = ("same-file" if same else "cross-file", tt, conf, v)
    rows[k] += 1
    ex.append((k, f"{sp}:{sn}", f"{tp}:{tn}", meta))
print(corpus)
for k, v in sorted(rows.items()): print("  ", k, v)
for e in ex:
    if e[0][0] == "cross-file": print("     ", e)
