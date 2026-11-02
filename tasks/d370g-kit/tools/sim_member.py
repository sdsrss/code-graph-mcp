import json, sqlite3, sys, collections
# sim_member.py JUDGED DB: for ext/member/same/method edges, verdict split by candidate predicates
d = json.load(open(sys.argv[1])); db = sqlite3.connect(f"file:{sys.argv[2]}?mode=ro", uri=True)
B = set()
for t in (dict, list, set, frozenset, str, bytes, bytearray, tuple, int, float, object, type):
    B |= {n for n in dir(t)}
import io; B |= set(dir(io.TextIOWrapper)) | set(dir(io.BufferedReader))
ids = collections.defaultdict(list)
for nid, path, line, name, q, typ, fid in db.execute("SELECT n.id, f.path, n.start_line, n.name, n.qualified_name, n.type, n.file_id FROM nodes n JOIN files f ON f.id=n.file_id"):
    ids[(path, line)].append((nid, name, q, typ, fid))
cnt = collections.Counter((fid, name) for (nid, name, fid) in db.execute("SELECT id, name, file_id FROM nodes WHERE type='method'"))
def node(at, name):
    path, line = at.rsplit(":", 1); c = ids.get((path, int(line)), [])
    m = [x for x in c if name in (x[1], x[2])] or c
    return m[0] if m else None
meta = {}
for s, t, m in db.execute("SELECT source_id, target_id, coalesce(metadata,'') FROM edges WHERE relation='calls' AND confidence='extracted'"):
    meta[(s, t)] = m
agg = collections.defaultdict(collections.Counter)
for e in d:
    if e["tier"] != "extracted": continue
    s = node(e["caller_at"], e["caller"]); t = node(e["callee_at"], e["callee"])
    if not s or not t or meta.get((s[0], t[0])) != '{"q":"member"}' or t[3] != "method" or s[4] != t[4]: continue
    ok = e["verdict"] == "correct"; name = t[1]
    for f, v in (("builtin_name", name in B), ("dunder", name.startswith("__")), ("same_name_methods>1", cnt[(t[4], name)] > 1), ("all", True)):
        agg[(f, v)]["ok" if ok else "bad"] += 1
for k, c in sorted(agg.items()): print(f"{k[0]:22s} {str(k[1]):5s} ok {c['ok']:5d} bad {c['bad']:5d}")
