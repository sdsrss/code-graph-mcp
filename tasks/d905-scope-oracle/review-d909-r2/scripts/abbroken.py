import os, shutil, sqlite3, subprocess, sys
R = "/var/tmp/d909-review2"
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
Q = """SELECT e.relation, sf.path, sn.name, sn.start_line, sn.end_line, tf.path, tn.name, tn.type, tn.start_line, e.confidence, COALESCE(e.metadata,'')
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
src, a, b, work = sys.argv[1:5]
out = {}
for arm in (a, b):
    d = os.path.join(work, arm); assert d.startswith(R + "/")
    shutil.rmtree(d, ignore_errors=True); shutil.copytree(src, d)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    r = subprocess.run([f"{R}/bin/{arm}", "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr[-500:]
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    from collections import Counter
    out[arm] = Counter(con.execute(Q)); con.close()
oa, ob = out[a] - out[b], out[b] - out[a]
print(f"{a}: {sum(out[a].values())} edges, {b}: {sum(out[b].values())}; {a}-only {sum(oa.values())}, {b}-only {sum(ob.values())}")
for tag, s in ((a + "-only", oa), (b + "-only", ob)):
    for k in sorted(s):
        print(tag, k)
