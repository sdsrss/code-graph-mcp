"""Classify wrong judged edges: is the callee a function nested in another function
(a local helper) while the file defines that name more than once?"""
import json, sqlite3, sys, collections
judged, db = sys.argv[1], sys.argv[2]
con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
FUN = ("function", "method")
defs = collections.defaultdict(list)  # (path, name) -> [(start, end, type)]
spans = collections.defaultdict(list)  # path -> [(start, end, type, name)]
for path, name, typ, s, e in con.execute("SELECT f.path, n.name, n.type, n.start_line, n.end_line FROM nodes n JOIN files f ON f.id=n.file_id"):
    if typ in FUN:
        defs[(path, name)].append((s, e, typ)); spans[path].append((s, e, typ, name))
def nested(path, line, name):
    me = [d for d in defs[(path, name)] if d[0] == line]
    if not me: return None
    s, e, _ = me[0]
    return any(ps <= s and e <= pe and (ps, pe) != (s, e) and pt == "function" for ps, pe, pt, _ in spans[path])
c = collections.Counter(); tot = collections.Counter()
for j in json.load(open(judged)):
    if j["tier"] == "ambiguous": continue
    tot[(j["tier"], j["verdict"])] += 1
    if j["verdict"] == "correct": continue
    cf, cl = j["callee_at"].rsplit(":", 1); sf = j["caller_at"].rsplit(":", 1)[0]
    multi = len(defs[(cf, j["callee"])]) > 1
    nst = nested(cf, int(cl), j["callee"])
    if sf != cf: k = "cross-file"
    elif multi and nst: k = "same-file: name defined >1x, callee is a nested local fn"
    elif multi: k = "same-file: name defined >1x (not nested)"
    else: k = "same-file: other"
    c[(j["tier"], k)] += 1
print(dict(tot))
for k, v in sorted(c.items()): print(v, k)
