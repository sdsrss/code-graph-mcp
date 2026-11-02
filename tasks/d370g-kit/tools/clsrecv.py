import json, sqlite3, sys, collections, re
# clsrecv.py REPO JUDGED: default-floor {"q":"member"} edges whose call sites are `X.m(` with X a class name
root = sys.argv[1]; db = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
verd = {(e["caller_at"], e["callee_at"]): e["verdict"] for e in json.load(open(sys.argv[2]))}
classes = collections.defaultdict(set); allcls = set()
for fid, name, q in db.execute("SELECT file_id, name, coalesce(qualified_name,name) FROM nodes WHERE type='class'"):
    allcls.add(name)
    if q == name: classes[fid].add(name)
srcs = {}
def body(path, a, b):
    if path not in srcs: srcs[path] = open(f"{root}/{path}", errors="replace").read().split("\n")
    return "\n".join(srcs[path][a-1:b])
c = collections.Counter()
q = """SELECT s.name, sf.path, s.start_line, s.end_line, s.file_id, t.name, t.start_line, tf.path, e.confidence
 FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id JOIN nodes t ON t.id=e.target_id JOIN files tf ON tf.id=t.file_id
 WHERE e.relation='calls' AND e.confidence!='ambiguous' AND e.metadata='{"q":"member"}' AND sf.path LIKE '%.py'"""
for sn, sp, s0, s1, sfid, tn, t0, tp, conf in db.execute(q):
    txt = body(sp, s0, s1)
    recvs = set(re.findall(r"(?<![\w.])([A-Za-z_]\w*)\s*\.\s*" + re.escape(tn) + r"\s*\(", txt))
    if not recvs: kind = "no-ident-recv"
    elif all(r in classes[sfid] for r in recvs): kind = "same-file-class"
    elif all(r in allcls for r in recvs): kind = "project-class-name"
    elif any(r in allcls for r in recvs): kind = "mixed"
    else: kind = "other"
    v = verd.get((f"{sp}:{s0}", f"{tp}:{t0}"), "-"); v = v if v in ("-", "correct") else "wrong"
    c[(conf[:3], "same" if sp == tp else "cross", kind, v)] += 1
for k, n in sorted(c.items(), key=lambda kv: (kv[0][2], -kv[1])): print(f"{n:6d}", k)
