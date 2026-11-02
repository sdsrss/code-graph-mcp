import json, sqlite3, sys, collections
# classify.py JUDGED DB: verdicts at the default floor by (tier, call-shape qualifier, same-file, target type)
d = json.load(open(sys.argv[1])); db = sqlite3.connect(f"file:{sys.argv[2]}?mode=ro", uri=True)
ids = collections.defaultdict(list)
for nid, path, line, name, q, typ in db.execute("SELECT n.id, f.path, n.start_line, n.name, n.qualified_name, n.type FROM nodes n JOIN files f ON f.id=n.file_id"):
    ids[(path, line)].append((nid, name, q, typ))
def node(at, name):
    path, line = at.rsplit(":", 1)
    c = ids.get((path, int(line)), [])
    m = [x for x in c if name in (x[1], x[2])] or c
    return m[0] if m else None
edges = collections.defaultdict(list)
for s, t, meta, conf in db.execute("SELECT source_id, target_id, coalesce(metadata,''), confidence FROM edges WHERE relation='calls'"):
    edges[(s, t)].append((meta, conf))
def shape(meta):
    if not meta: return "bare"
    try: m = json.loads(meta)
    except Exception: return "?" + meta[:20]
    q = m.get("q", "?")
    if q == "module" and "attr" in m: q = "module.attr"
    if m.get("amb"): q += "+amb"
    return q
agg = collections.defaultdict(collections.Counter); miss = 0
tiers = (sys.argv[3] if len(sys.argv) > 3 else "extracted,inferred").split(",")
for e in d:
    if e["tier"] not in tiers: continue
    s = node(e["caller_at"], e["caller"]); t = node(e["callee_at"], e["callee"])
    if not s or not t or (s[0], t[0]) not in edges: miss += 1; continue
    meta = max(edges[(s[0], t[0])], key=lambda x: x[1] == e["tier"])[0]
    same = e["caller_at"].rsplit(":",1)[0] == e["callee_at"].rsplit(":",1)[0]
    agg[(e["tier"][:3], shape(meta), "same" if same else "cross", t[3])]["ok" if e["verdict"] == "correct" else "bad"] += 1
print("unmatched judged edges:", miss)
for k, c in sorted(agg.items(), key=lambda kv: -sum(kv[1].values())):
    n = sum(c.values()); print(f"{'/'.join(k):45s} {c['ok']:5d} ok {c['bad']:4d} bad  P={c['ok']/n:.1%}")
