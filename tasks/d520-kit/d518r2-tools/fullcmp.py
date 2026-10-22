#!/usr/bin/env python3
"""fullcmp.py <corpus-src> <name> arm...: copy the corpus (git archive HEAD, or a plain
copy with --plain), git init, full-index it with each arm's binary, and record:
edges_created from --json, health-check --json edges, count_user_edges-equivalent
SQL, wall/cpu, and a canonical dump of every edge to full/<name>/<arm>.edges."""
import os, sys, subprocess, shutil, sqlite3, time, resource, json, hashlib
R = "/var/tmp/d518r2"
args = sys.argv[1:]
plain = "--plain" in args
args = [a for a in args if a != "--plain"]
src, name, arms = args[0], args[1], args[2:]
env = dict(os.environ, HOME=R + "/home", CODE_GRAPH_NO_AUTO_UPDATE="1", CODE_GRAPH_NO_UPDATE_CHECK="1")
Q = """select coalesce(sf.path,'?'), sn.name, coalesce(sn.qualified_name,''), sn.type, sn.start_line,
              coalesce(tf.path,'?'), tn.name, coalesce(tn.qualified_name,''), tn.type, tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
SYN = "(relation = 'imports' AND COALESCE(metadata, '') LIKE '%\"attr\":%')"
for arm in arms:
    d = f"{R}/full/{name}/{arm}"
    if os.path.exists(d): shutil.rmtree(d)
    os.makedirs(d)
    if plain:
        subprocess.run(f"cd {src} && tar --exclude=./.code-graph --exclude=./target --exclude=./.git -cf - . | tar -x -C {d}", shell=True, check=True)
    else:
        subprocess.run(f"git -C {src} archive HEAD | tar -x -C {d}", shell=True, check=True)
    subprocess.run(["git", "init", "-q"], cwd=d, check=True)
    t0 = time.time(); r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed", "--json"], cwd=d, env=env, capture_output=True, text=True)
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    wall = time.time() - t0
    if r.returncode: print("FAIL", arm, r.stderr[-2000:]); sys.exit(2)
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    j = json.loads([l for l in r.stdout.splitlines() if l.startswith("{")][-1])
    h = subprocess.run([f"{R}/bin/cgm-{arm}", "health-check", "--json"], cwd=d, env=env, capture_output=True, text=True)
    hj = json.loads(h.stdout)
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    allc = con.execute("select count(*) from edges").fetchone()[0]
    syn = con.execute(f"select count(*) from edges where {SYN}").fetchone()[0]
    rows = sorted("\t".join(map(str, x)) for x in con.execute(Q))
    con.close()
    txt = "\n".join(rows) + "\n"
    open(f"{R}/full/{name}/{arm}.edges", "w").write(txt)
    print(f"{name} {arm}: edges_created={j['edges_created']} health_edges={hj['edges']} all={allc} synthetic={syn} "
          f"user={allc-syn} files={j['files_indexed']} md5={hashlib.md5(txt.encode()).hexdigest()[:12]} wall={wall:.2f}s cpu={cpu:.2f}s", flush=True)
    shutil.rmtree(d)
