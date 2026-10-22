import sqlite3, sys, collections, json
def roots(paths):
    pk = {p[:-len("/__init__.py")] for p in paths if p.endswith("/__init__.py")}
    r = {""}
    for p in paths:
        chain = []; d = p.rsplit("/", 1)[0] if "/" in p else None
        while d is not None:
            chain.append(d); d = d.rsplit("/", 1)[0] if "/" in d else None
        for d in reversed(chain):
            if d in pk: break
            r.add(d)
    return r
for c in sys.argv[1:]:
    con = sqlite3.connect(f"file:{c}-head/.code-graph/index.db?mode=ro", uri=True)
    paths = [r[0] for r in con.execute("select path from files where path like '%.py'")]
    rt = roots(paths); m = collections.defaultdict(set)
    for p in paths:
        s = p[:-12] if p.endswith("/__init__.py") else p[:-3]
        for root in rt:
            rel = s if root == "" else (s[len(root)+1:] if s.startswith(root + "/") else None)
            if rel: m[rel.replace("/", ".")].add(p)
    multi = {k: v for k, v in m.items() if len(v) > 1}
    used = collections.Counter()
    for (meta,) in con.execute("select distinct metadata from edges where relation='imports' and metadata like '%python_module%'"):
        mod = json.loads(meta).get("python_module")
        if mod in multi: used[mod] += 1
    print(c, "module paths naming >=2 files:", len(multi), "used by imports:", len(used), list(used.items())[:8])
    print("   e.g.", [(k, sorted(v)) for k, v in list(multi.items())[:4]])
