import json, sqlite3, sys, collections
# sim2.py JUDGED DB LANG(py|js): untyped member-call edges (q=member) at extracted/inferred, by same/cross file and builtin-name predicate
d = json.load(open(sys.argv[1])); db = sqlite3.connect(f"file:{sys.argv[2]}?mode=ro", uri=True)
B = set(json.load(open(f"/var/tmp/d370/{'py' if sys.argv[3]=='py' else 'js'}_builtin_names.json")))
ids = collections.defaultdict(list)
for nid, path, line, name, q, typ, fid in db.execute("SELECT n.id, f.path, n.start_line, n.name, n.qualified_name, n.type, n.file_id FROM nodes n JOIN files f ON f.id=n.file_id"):
    ids[(path, line)].append((nid, name, q, typ, fid))
def node(at, name):
    path, line = at.rsplit(":", 1); c = ids.get((path, int(line)), [])
    m = [x for x in c if name in (x[1], x[2])] or c
    return m[0] if m else None
meta = collections.defaultdict(set)
for s, t, m, c in db.execute("SELECT source_id, target_id, coalesce(metadata,''), confidence FROM edges WHERE relation='calls'"):
    meta[(s, t, c)].add(m)
agg = collections.defaultdict(collections.Counter); names = collections.defaultdict(collections.Counter)
for e in d:
    if e["tier"] not in ("extracted", "inferred"): continue
    s = node(e["caller_at"], e["caller"]); t = node(e["callee_at"], e["callee"])
    if not s or not t: continue
    ms = meta.get((s[0], t[0], e["tier"]), set())
    if not any(m.startswith('{"q":"member"') or m.startswith('{"ur"') for m in ms): continue
    same = "same" if s[4] == t[4] else "cross"; bi = t[1] in B; ok = e["verdict"] == "correct"
    agg[(e["tier"][:3], same, bi)]["ok" if ok else "bad"] += 1
    if bi: names[(same, t[1])]["ok" if ok else "bad"] += 1
for k, c in sorted(agg.items()): print(f"{'/'.join(map(str,k)):22s} ok {c['ok']:5d} bad {c['bad']:5d}")
if len(sys.argv) > 4:
    print("builtin-named, most correct first:")
    for k, c in sorted(names.items(), key=lambda kv: -kv[1]['ok'])[:15]: print("  ", k, dict(c))
