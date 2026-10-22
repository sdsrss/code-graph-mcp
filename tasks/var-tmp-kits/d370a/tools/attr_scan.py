#!/usr/bin/env python3
"""attr_scan.py <index.db>: what a module-attribute lookup (a node X defined at
module level in P's files, else the targets of P's own `imports` edges to an X)
would bind, against today's edges, for (1) absolute `from P import X` import
edges and (2) Module(v) calls `v.X()` with v a project module."""
import collections, json, sqlite3, sys
con = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
files = {r[0]: r[1] for r in con.execute("select path, id from files where language='python'")}
mod = collections.defaultdict(set)
for f in files:
    p = f[:-3]
    if p.endswith("/__init__"): p = p[: -len("/__init__")]
    parts = p.split("/")
    for i in range(len(parts)):
        mod[".".join(parts[i:])].add(f)
toplevel = collections.defaultdict(set)  # (file, name) -> ids at module level
for fid_path, name, nid, qn in con.execute("""select f.path, n.name, n.id, coalesce(n.qualified_name,n.name) from nodes n join files f on f.id=n.file_id where f.language='python'"""):
    if "." not in qn and name != "<module>": toplevel[(fid_path, name)].add(nid)
reexp = collections.defaultdict(set)  # (file, name) -> targets of its non-module imports
for sp, tname, tid in con.execute("""select sf.path, tn.name, tn.id from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id
   where e.relation='imports' and sf.language='python' and e.metadata like '%python_module%' and e.metadata not like '%is_module_import%' and sn.name='<module>'"""):
    reexp[(sp, tname)].add(tid)
def attr(F, x, depth=0):
    out = set()
    for f in F: out |= toplevel.get((f, x), set())
    if out: return out, "defined"
    for f in F: out |= reexp.get((f, x), set())
    return out, ("reexport" if out else "none")
nodeinfo = {r[0]: (r[1], r[2]) for r in con.execute("select n.id, f.path, n.name from nodes n join files f on f.id=n.file_id")}
def judge(rows, label):
    c = collections.Counter(); samples = collections.defaultdict(list)
    g = collections.defaultdict(set)
    for key, tid in rows: g[key].add(tid)
    for (sp, v, x), tids in g.items():
        F = mod.get(v, set())
        if not F: c["v_not_project"] += 1; continue
        want, how = attr(F, x)
        if how == "none": c["lookup_none"] += 1; samples["none"].append((sp, v, x, sorted(nodeinfo[t][0] for t in tids))); continue
        k = how + (":same" if tids == want else (":superset" if want < tids else (":disjoint" if not (want & tids) else ":overlap")))
        c[k] += 1; samples[k].append((sp, v, x, sorted(nodeinfo[t][0] for t in tids), sorted(nodeinfo[t][0] for t in want)))
    print(label, dict(c))
    for k, v in samples.items():
        if k.endswith(":same"): continue
        print("  ", k, len(v)); [print("      ", s) for s in v[:8]]
imp = [((sp, json.loads(m)["python_module"], tn), tid) for sp, tn, tid, m in con.execute("""select sf.path, tn.name, tn.id, e.metadata from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id
   where e.relation='imports' and sf.language='python' and e.metadata like '%python_module%' and e.metadata not like '%is_module_import%' and e.metadata not like '%"python_module":".%'""")]
judge(imp, "absolute from-imports (grouped by importer,module,name):")
calls = [((sp, json.loads(m)["v"], tn), tid) for sp, tn, tid, m in con.execute("""select sf.path, tn.name, tn.id, e.metadata from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id
   where e.relation='calls' and json_extract(e.metadata,'$.q')='module'""")]
judge(calls, "module-attribute calls (grouped by caller file,module,name):")
