#!/usr/bin/env python3
"""full.py <corpus-src-dir> <name> [arm ...]: git-archive the corpus into
/var/tmp/d520-review/full/<name>/<arm>, git init, full index with that arm's binary,
dump every edge canonically to <dir>.edges (TSV), print counts + diff summary."""
import os, sys, subprocess, shutil, sqlite3, time, resource
R = "/var/tmp/d520-review"
src, name = sys.argv[1], sys.argv[2]
arms = sys.argv[3:] or ["base", "head"]
env = dict(os.environ, HOME=R + "/home", CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_NO_UPDATE_CHECK="1")
Q = """select coalesce(sf.path,'?'), sn.name, sn.start_line, coalesce(tf.path,'?'), tn.name, tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
for arm in arms:
    d = f"{R}/full/{name}/{arm}"
    if os.path.exists(d): shutil.rmtree(d)
    os.makedirs(d)
    subprocess.run(f"git -C {src} archive HEAD | tar -x -C {d}", shell=True, check=True)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    t0 = time.time(); r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    if r.returncode: print("FAIL", arm, r.stderr[-2000:]); sys.exit(2)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = sorted("\t".join(map(str, x)) for x in con.execute(Q))
    con.close()
    open(d + ".edges", "w").write("\n".join(rows) + "\n")
    print(f"{name} {arm}: edges={len(rows)} wall={time.time()-t0:.2f}s cpu={cpu:.2f}s")
