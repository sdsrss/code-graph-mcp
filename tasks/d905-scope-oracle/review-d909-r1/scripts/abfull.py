#!/usr/bin/env python3
"""abfull.py <corp-dir> <work> [names]: copy each corpus once per arm, fresh
index with base and head, dump every edge (with start lines) to
<work>/<n>-<arm>.edges.tsv, keep the copies (for later queries)."""
import os, shutil, sqlite3, subprocess, sys, time

BIN = {"base": "/var/tmp/d909-review/bin/base", "head": "/var/tmp/d909-review/bin/head"}
ENV = dict(os.environ, CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_DISABLE_MODEL_DOWNLOAD="1")
ROWS = """SELECT e.relation, sf.path, sn.type, COALESCE(sn.qualified_name, sn.name), sn.start_line,
 tf.path, tn.type, COALESCE(tn.qualified_name, tn.name), tn.start_line, e.confidence, COALESCE(e.metadata,''), sf.language
 FROM edges e JOIN nodes sn ON sn.id=e.source_id JOIN files sf ON sf.id=sn.file_id
 JOIN nodes tn ON tn.id=e.target_id JOIN files tf ON tf.id=tn.file_id"""

corp, work = sys.argv[1], os.path.abspath(sys.argv[2])
names = sys.argv[3:] or ["std", "nltk", "flask", "cgm", "daagu"]
os.makedirs(work, exist_ok=True)
for n in names:
    for arm in ("base", "head"):
        d = os.path.join(work, f"{n}-{arm}")
        assert d.startswith("/var/tmp/d909-review/")
        shutil.rmtree(d, ignore_errors=True)
        shutil.copytree(os.path.join(corp, n), d, symlinks=True)
        shutil.rmtree(os.path.join(d, ".code-graph"), ignore_errors=True)
        if not os.path.exists(os.path.join(d, ".git")):
            subprocess.run(["git", "init", "-q"], cwd=d, check=True)
        t = time.monotonic()
        r = subprocess.run([BIN[arm], "incremental-index", "--no-embed", "--quiet"], cwd=d, env=ENV,
                           capture_output=True, text=True)
        secs = time.monotonic() - t
        if r.returncode:
            raise SystemExit(f"{d}: {r.stderr[-800:]}")
        con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
        rows = list(con.execute(ROWS))
        con.close()
        with open(os.path.join(work, f"{n}-{arm}.edges.tsv"), "w") as fh:
            for row in rows:
                fh.write("\t".join(map(str, row)) + "\n")
        print(f"{n} {arm}: {len(rows)} edges, {secs:.2f}s", flush=True)
