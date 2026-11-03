"""D#714 A3: simulate binding a call among same-file same-name definitions by
lexical scope, using only node spans (no parser change). Visible: a candidate
whose enclosing scope (innermost function/method/class node containing it, else
the file) contains the caller; kept: the visible ones with the innermost scope.
  sim_span_scope.py JUDGED.json INDEX.db"""
import json, sqlite3, sys, collections
judged, db = sys.argv[1], sys.argv[2]
con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
SCOPE = ("function", "method", "class")
nodes = collections.defaultdict(list)  # path -> [(start, end, type, name)]
for path, name, typ, s, e in con.execute("SELECT f.path, n.name, n.type, n.start_line, n.end_line FROM nodes n JOIN files f ON f.id=n.file_id"):
    nodes[path].append((s, e, typ, name))
def scope_of(path, s, e):
    inner = [(ps, pe) for ps, pe, pt, _ in nodes[path] if pt in SCOPE and ps <= s and e <= pe and (ps, pe) != (s, e)]
    return min(inner, key=lambda x: x[1] - x[0]) if inner else (0, 10**9)
def span_at(path, line, name=None):
    c = [(s, e) for s, e, t, n in nodes[path] if s == line and (name is None or n == name)]
    return c[0] if c else None
groups = collections.defaultdict(list)
for j in json.load(open(judged)):
    if j["tier"] == "ambiguous": continue
    cf, cl = j["callee_at"].rsplit(":", 1); sf, sl = j["caller_at"].rsplit(":", 1)
    if cf != sf: continue
    groups[(sf, int(sl), j["caller"], j["callee"])].append((int(cl), j["verdict"]))
stats = collections.Counter(); shapes = collections.Counter()
for (path, sl, caller, callee), edges in groups.items():
    cands = [(s, e) for s, e, t, n in nodes[path] if n == callee and t in ("function", "method")]
    if len(cands) < 2: continue
    cs = span_at(path, sl, caller) or (0, 10**9)
    vis = []
    for s, e in cands:
        ps, pe = scope_of(path, s, e)
        if ps <= cs[0] and cs[1] <= pe: vis.append(((s, e), pe - ps))
    if not vis:
        shapes["no visible candidate"] += 1; kept = {s for s, _ in cands}
    else:
        m = min(w for _, w in vis); kept = {s for (s, _), w in vis if w == m}
        shapes["one kept" if len(kept) == 1 else f"{len(kept)} kept (same scope)"] += 1
    for line, verdict in edges:
        stats[(verdict, "kept" if line in kept else "dropped")] += 1
print("call groups with same-file same-name candidates:", sum(shapes.values()), dict(shapes))
for k, v in sorted(stats.items()): print(v, k)
