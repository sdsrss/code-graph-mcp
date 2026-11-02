import json, sqlite3, sys, collections, re
# modsplit.py REPO [JUDGED]: default-floor calls with {"q":"module"} metadata whose target is in the caller's own file
root = sys.argv[1]; db = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
verd = {}
if len(sys.argv) > 2:
    for e in json.load(open(sys.argv[2])):
        verd[(e["caller_at"], e["callee_at"])] = e["verdict"]
def modname(path):
    p = path[:-3] if path.endswith(".py") else path
    p = p.replace("/", ".")
    return p[:-9] if p.endswith(".__init__") else p
src_cache = {}
def lines(path):
    if path not in src_cache: src_cache[path] = open(f"{root}/{path}", errors="replace").read().split("\n")
    return src_cache[path]
c = collections.Counter(); ex = collections.defaultdict(list)
q = """SELECT e.metadata, s.name, sf.path, s.start_line, s.end_line, t.name, t.type, t.start_line, e.confidence
 FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id JOIN nodes t ON t.id=e.target_id
 WHERE e.relation='calls' AND e.confidence!='ambiguous' AND s.file_id=t.file_id AND e.metadata LIKE '%"q":"module"%'"""
for meta, sn, path, s0, s1, tn, tt, t0, conf in db.execute(q):
    m = json.loads(meta); v = m["v"]; own = modname(path)
    rel = "self" if v == own else ("ancestor" if own.startswith(v + ".") else "other")
    # call text: find `<root>.….<tn>(` in the caller span
    body = "\n".join(lines(path)[s0-1:s1])
    root_names = set(re.findall(r"\b(\w+)(?:\s*\.\s*\w+)*\s*\.\s*" + re.escape(tn) + r"\s*\(", body))
    chain = "attr" if "attr" in m else ("deep" if re.search(r"\b\w+\s*\.\s*\w+\s*\.\s*" + re.escape(tn) + r"\s*\(", body) else "declined")
    vv = verd.get((f"{path}:{s0}", f"{path}:{t0}"), "-")
    k = (rel, chain, tt, vv if vv in ("-", "correct") else "wrong")
    c[k] += 1
    if len(ex[k]) < 3: ex[k].append(f"{sn}@{path}:{s0} -> {tn}@{t0} {meta}")
for k, n in sorted(c.items(), key=lambda kv: -kv[1]): print(n, k, ex[k][0][:150])
