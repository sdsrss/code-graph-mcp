#!/usr/bin/env python3
"""corpus_parity.py <src_corpus> <name> <edits.py> <bin...>: copy the corpus (no .git/.code-graph),
git init, full index, apply each run of edits (EDITS: list of runs; a run is a list of
(path, op, a, b): op 'replace' a->b once, 'append' a, 'delete', 'write' a) + incremental-index,
then full-index the final tree in a fresh copy and compare every edge (lines, metadata, confidence)."""
import collections, os, shutil, sqlite3, subprocess, sys, time
src, name, edits_file, *bins = sys.argv[1:]
E = {}; exec(open(edits_file).read(), E)
R = "/var/tmp/d370a-review2"
env = dict(os.environ, HOME=f"{R}/fxhome", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
Q = """select coalesce(sf.path,'?'), sn.name, sn.start_line, coalesce(tf.path,'?'), tn.name, tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def copy(dst):
    if os.path.exists(dst): shutil.rmtree(dst)
    os.makedirs(dst)
    subprocess.run(f"tar -C {src} --exclude=./.code-graph --exclude=./.git -cf - . | tar -x -C {dst}", shell=True, check=True)
    subprocess.run(["git", "init", "-q"], cwd=dst, check=True)
def index(b, d):
    r = subprocess.run([f"{R}/bin/{b}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
    if r.returncode: print("FAIL", r.stderr[-1500:]); sys.exit(2)
def apply(d, run):
    for path, op, *args in run:
        fp = os.path.join(d, path)
        if op == "delete": os.remove(fp); continue
        if op == "write":
            os.makedirs(os.path.dirname(fp), exist_ok=True); open(fp, "w").write(args[0])
        else:
            s = open(fp).read()
            if op == "replace":
                assert s.count(args[0]) >= 1, (path, args[0]); s = s.replace(args[0], args[1], 1)
            elif op == "append": s += args[0]
            open(fp, "w").write(s)
        t = time.time() + 5; os.utime(fp, (t, t))
def dump(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    rows = collections.Counter(tuple(r) for r in con.execute(Q)); con.close(); return rows
rc = 0
for b in bins:
    inc = f"{R}/cpar/{name}/{b}/inc"; ctl = f"{R}/cpar/{name}/{b}/ctl"
    copy(inc); index(b, inc)
    for run in E["EDITS"]:
        time.sleep(0.05); apply(inc, run); index(b, inc)
    copy(ctl); [apply(ctl, run) for run in E["EDITS"]]; index(b, ctl)
    a, c = dump(inc), dump(ctl)
    oi, oc = a - c, c - a
    print(f"[{b}] {name}: edges inc={sum(a.values())} rebuild={sum(c.values())} equal={not oi and not oc} inc-only={sum(oi.values())} rebuild-only={sum(oc.values())}")
    for r in list(oi)[:12]: print("   INC-ONLY    ", r)
    for r in list(oc)[:12]: print("   REBUILD-ONLY", r)
    if oi or oc: rc = 1
sys.exit(rc)
