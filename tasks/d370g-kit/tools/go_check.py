import sqlite3, re, sys, os, collections
# go_check.py REPO: call edges at the default floor whose every call site of the
# target name in the caller is `pkg.Name(` with pkg an import of an external
# (non-project) package in the caller's file.
root = sys.argv[1]
db = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
proj_pkgs = set()
for dp, dn, fn in os.walk(root):
    if "/." in dp: continue
    for f in fn:
        if f.endswith(".go"):
            for line in open(os.path.join(dp, f), errors="replace"):
                m = re.match(r"package\s+(\w+)", line)
                if m: proj_pkgs.add(m.group(1)); break
ext_imports = collections.defaultdict(set)  # file_id -> imported external pkg names
for fid, name in db.execute("""SELECT s.file_id, t.name FROM edges e JOIN nodes s ON s.id=e.source_id
    JOIN nodes t ON t.id=e.target_id JOIN files tf ON tf.id=t.file_id
    WHERE e.relation='imports' AND tf.path='<external>'"""):
    if name not in proj_pkgs: ext_imports[fid].add(name)
rows = db.execute("""SELECT e.confidence, s.name, sf.path, s.file_id, s.code_content, t.name, t.type, tf.path, t.start_line
  FROM edges e JOIN nodes s ON s.id=e.source_id JOIN files sf ON sf.id=s.file_id
  JOIN nodes t ON t.id=e.target_id JOIN files tf ON tf.id=t.file_id
  WHERE e.relation='calls' AND e.confidence IN ('extracted','inferred') AND sf.path LIKE '%.go'""").fetchall()
bad = collections.Counter(); total = collections.Counter(); ex = []
for conf, sname, spath, sfid, code, tname, ttype, tpath, tline in rows:
    total[conf] += 1
    if tpath == "<external>": continue
    quals = re.findall(r"(?:(\w+)\s*\.\s*)?\b" + re.escape(tname) + r"\s*[\(\{]", code)
    quals = [q for q in quals]
    if quals and all(q and q in ext_imports[sfid] for q in quals):
        bad[conf] += 1; ex.append((conf, f"{sname}@{spath}", f"{sorted(set(quals))}.{tname} -> {ttype} {tname}@{tpath}:{tline}"))
print("default-floor call edges:", dict(total), " external-pkg-qualified bound to project:", dict(bad))
for e in ex[:40]: print(" ", *e)
