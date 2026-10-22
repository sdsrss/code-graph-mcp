#!/usr/bin/env python3
"""reexp_scan.py <index.db>: Python `from P import X` import edges whose target
is not in P's own file(s) (fell through to the name chain), split by whether P's
file re-exports X through one of its own import edges; and Module(v) calls."""
import collections, json, sqlite3, sys
con = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
files = [r[0] for r in con.execute("select path from files where language='python'")]
# dotted module -> files (root-anchored only; good enough for a scan)
mod = collections.defaultdict(set)
for f in files:
    p = f[:-3]
    if p.endswith("/__init__"): p = p[: -len("/__init__")]
    parts = p.split("/")
    for i in range(len(parts)):
        mod[".".join(parts[i:])].add(f)
def resolve(pm, importer):
    if pm.startswith("."):
        rest = pm[1:]; d = importer.rsplit("/", 1)[0] if "/" in importer else ""
        while rest.startswith("."):
            rest = rest[1:]; d = d.rsplit("/", 1)[0] if "/" in d else ""
        path = "/".join(x for x in [d, rest.replace(".", "/")] if x)
        return {f for f in (path + ".py", path + "/__init__.py") if f in files}
    return mod.get(pm, set())
rows = con.execute("""select sf.path, tn.name, coalesce(tf.path,'?'), e.metadata, e.confidence
  from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id
  join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id
  where e.relation='imports' and sf.language='python' and e.metadata like '%python_module%'
    and e.metadata not like '%is_module_import%'""").fetchall()
reexp = collections.defaultdict(set)  # file -> names its <module> imports
for sp, tname, tp, meta, conf in rows: reexp[sp].add(tname)
c = collections.Counter(); ex = collections.defaultdict(list)
for sp, tname, tp, meta, conf in rows:
    pm = json.loads(meta)["python_module"]; F = resolve(pm, sp)
    if not F: c["external_or_unmapped"] += 1; continue
    if tp in F: c["bound_in_module"] += 1; continue
    k = "reexported_by_module" if any(tname in reexp.get(f, ()) for f in F) else "not_reexported"
    c[k + ("_relative" if pm.startswith(".") else "_absolute")] += 1
    ex[k].append((sp, pm, tname, tp))
print(dict(c))
for k, v in ex.items():
    print(k, len(v)); [print("   ", x) for x in v[:12]]
mc = con.execute("""select count(*), sum(tf.id=sf.id), sum(e.confidence='extracted'), sum(e.confidence='inferred'), sum(e.confidence='ambiguous')
  from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id
  join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
  where e.relation='calls' and json_extract(e.metadata,'$.q')='module'""").fetchone()
print("module-meta calls (total, same-file, extracted, inferred, ambiguous):", mc)
