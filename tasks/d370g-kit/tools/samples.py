import json, sqlite3, sys, collections, random
# samples.py JUDGED DB CLASS N : sample judged edges of one class (tier/shape/same/type), with the call line text
sys.path.insert(0, "/var/tmp/d370")
d = json.load(open(sys.argv[1])); db = sqlite3.connect(f"file:{sys.argv[2]}?mode=ro", uri=True)
want = sys.argv[3]; n = int(sys.argv[4]); verdict = sys.argv[5] if len(sys.argv) > 5 else None
root = sys.argv[2].rsplit("/.code-graph", 1)[0]
ids = collections.defaultdict(list)
for nid, path, line, end, name, q, typ in db.execute("SELECT n.id, f.path, n.start_line, n.end_line, n.name, n.qualified_name, n.type FROM nodes n JOIN files f ON f.id=n.file_id"):
    ids[(path, line)].append((nid, name, q, typ, end))
def node(at, name):
    path, line = at.rsplit(":", 1); c = ids.get((path, int(line)), [])
    m = [x for x in c if name in (x[1], x[2])] or c
    return m[0] if m else None
edges = collections.defaultdict(list)
for s, t, meta, conf in db.execute("SELECT source_id, target_id, coalesce(metadata,''), confidence FROM edges WHERE relation='calls'"):
    edges[(s, t)].append((meta, conf))
def shape(meta):
    if not meta: return "bare"
    m = json.loads(meta); q = m.get("q", "?")
    if q == "module" and "attr" in m: q = "module.attr"
    if m.get("amb"): q += "+amb"
    return q
out = []
for e in d:
    s = node(e["caller_at"], e["caller"]); t = node(e["callee_at"], e["callee"])
    if not s or not t or (s[0], t[0]) not in edges: continue
    meta = max(edges[(s[0], t[0])], key=lambda x: x[1] == e["tier"])[0]
    same = e["caller_at"].rsplit(":",1)[0] == e["callee_at"].rsplit(":",1)[0]
    k = "/".join((e["tier"][:3], shape(meta), "same" if same else "cross", t[3]))
    if k != want or (verdict and (e["verdict"] == "correct") != (verdict == "ok")): continue
    path, l0 = e["caller_at"].rsplit(":", 1)
    lines = open(f"{root}/{path}", errors="replace").read().split("\n")[int(l0)-1:s[4]]
    site = [x.strip() for x in lines[1:] if e["callee"].split(".")[-1] + "(" in x][:2]
    out.append((e["verdict"][:5], e["caller"] + "@" + e["caller_at"], e["callee"] + "@" + e["callee_at"], meta, site))
random.seed(1); random.shuffle(out)
for o in out[:n]: print(*o[:4], "|", " || ".join(o[4])[:160])
