#!/usr/bin/env python3
"""abcorp.py <work> <corpus-dir>...: fresh index per arm ($ARMS) of a tar copy (no .git/.code-graph, git init);
every edge keyed (relation, both ends path:qname:type@line, confidence, metadata); per pair vs last arm: only-in counts by language."""
import os, shutil, sqlite3, subprocess, sys
from collections import Counter
R = "/var/tmp/agent-tmp-d909-review3"
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
ARMS = os.environ.get("ARMS", "v0.203.0 c52b115c f06f5e96").split()
Q = """SELECT e.relation, sf.path||':'||COALESCE(sn.qualified_name,sn.name)||':'||sn.type||'@'||sn.start_line,
 tf.path||':'||COALESCE(tn.qualified_name,tn.name)||':'||tn.type||'@'||tn.start_line, e.confidence, COALESCE(e.metadata,''), sf.language
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""
work = os.path.abspath(sys.argv[1]); assert work.startswith(R + "/")
for c in sys.argv[2:]:
    res = {}
    for a in ARMS:
        d = os.path.join(work, os.path.basename(c.rstrip("/")) + "-" + a)
        shutil.rmtree(d, ignore_errors=True); os.makedirs(d)
        subprocess.run(f"tar --exclude=.code-graph --exclude=.git --exclude=node_modules -C '{c}' -cf - . | tar -C '{d}' -xf -", shell=True, check=True)
        subprocess.run(["git", "init", "-q"], cwd=d, check=True)
        r = subprocess.run([f"{R}/bin/{a}", "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV, capture_output=True, text=True)
        if r.returncode: print("FAIL", c, a, r.stderr[-300:]); break
        con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
        res[a] = Counter(con.execute(Q)); con.close()
        shutil.rmtree(d, ignore_errors=True)
    head = ARMS[-1]
    langs = Counter(k[5] for k in res[head].elements())
    print(f"== {c}: edges per arm {[sum(res[a].values()) for a in ARMS]} by lang(head) {dict(langs.most_common(5))}")
    for a in ARMS[:-1]:
        lost = res[a] - res[head]; gained = res[head] - res[a]
        print(f"   {a} -> {head}: only-{a} {sum(lost.values())} {dict(Counter((k[0], k[5]) for k in lost.elements()))}; only-head {sum(gained.values())} {dict(Counter((k[0], k[5]) for k in gained.elements()))}")
