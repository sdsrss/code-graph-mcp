#!/usr/bin/env python3
"""nested_scan.py <index.db>: edges into Python class/function nodes nested in a
function body (span inside a function/method node of the same file), split by
relation, callee-metadata kind, and whether the source lies inside the target's
enclosing function (the only place a function-local name is visible)."""
import collections, json, sqlite3, sys
con = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
rows = con.execute("""select n.id, n.file_id, n.start_line, n.end_line, n.type, n.name from nodes n join files f on f.id=n.file_id where f.language='python'""").fetchall()
byfile = collections.defaultdict(list)
for r in rows: byfile[r[1]].append(r)
encl = {}  # nested node id -> (start,end) of innermost enclosing function/method
for fid, ns in byfile.items():
    funcs = [r for r in ns if r[4] in ("function", "method")]
    for n in ns:
        if n[4] not in ("class", "function"): continue
        outs = [o for o in funcs if o[0] != n[0] and o[2] < n[2] and o[3] >= n[3]]
        if outs:
            o = max(outs, key=lambda o: o[2]); encl[n[0]] = (o[2], o[3])
print("python class/function nodes:", sum(1 for r in rows if r[4] in ("class","function")), "nested in a function:", len(encl))
c = collections.Counter()
for sid, sfid, sstart, send, tid, tfid, rel, meta, conf in con.execute("""select e.source_id, sn.file_id, sn.start_line, sn.end_line, e.target_id, tn.file_id, e.relation, coalesce(e.metadata,''), e.confidence
   from edges e join nodes sn on sn.id=e.source_id join nodes tn on tn.id=e.target_id"""):
    if tid not in encl: continue
    q = ""
    if meta.startswith("{"):
        try: j = json.loads(meta); q = j.get("q") or ("ur" if "ur" in j else ("pm" if "python_module" in j else "?"))
        except Exception: q = "?"
    lo, hi = encl[tid]
    where = "cross-file" if sfid != tfid else ("inside-encl" if lo <= sstart and send <= hi else "same-file-outside")
    c[(rel, q or "bare", where, conf)] += 1
for k, v in sorted(c.items(), key=lambda x: -x[1]): print(f"  {v:5d}  {k}")
