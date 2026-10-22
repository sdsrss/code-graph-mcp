"""Shared helpers for corpus experiments."""
import collections, os, shutil, sqlite3, subprocess, time, resource
R = "/var/tmp/d543"
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
Q = """select coalesce(sf.path,'?'), sn.name, coalesce(sn.qualified_name,''), sn.type,
              coalesce(tf.path,'?'), tn.name, coalesce(tn.qualified_name,''), tn.type,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def copy(src, dst):
    if os.path.exists(dst): shutil.rmtree(dst)
    os.makedirs(dst)
    p1 = subprocess.Popen(["tar", "-C", src, "--exclude=./.code-graph", "-cf", "-", "."], stdout=subprocess.PIPE)
    subprocess.run(["tar", "-C", dst, "-xf", "-"], stdin=p1.stdout, check=True); p1.wait()
def index(binary, root, check=True):
    t0 = time.time(); r0 = resource.getrusage(resource.RUSAGE_CHILDREN)
    r = subprocess.run([binary, "incremental-index", "--no-embed"], cwd=root, env=env, capture_output=True, text=True, timeout=3600)
    r1 = resource.getrusage(resource.RUSAGE_CHILDREN)
    if r.returncode and check: raise SystemExit(f"index failed {root}: {r.stderr[-1500:]}")
    cpu = (r1.ru_utime - r0.ru_utime) + (r1.ru_stime - r0.ru_stime)
    return time.time() - t0, cpu, r
def dump(root):
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    rows = collections.Counter(tuple(r) for r in con.execute(Q)); con.close(); return rows
def fmt(k):
    return f"{k[0]}:{k[2] or k[1]}({k[3]}) -{k[8]}[{k[10]}]-> {k[4]}:{k[6] or k[5]}({k[7]}) {k[9]}"
def diff(a, f, limit=40, label=""):
    oa, of = a - f, f - a
    print(f"   {label} inc={sum(a.values())} fresh={sum(f.values())} only-inc={sum(oa.values())} only-fresh={sum(of.values())}")
    for k in sorted(oa)[:limit]: print("     +inc  ", fmt(k))
    for k in sorted(of)[:limit]: print("     +fresh", fmt(k))
    return oa, of
