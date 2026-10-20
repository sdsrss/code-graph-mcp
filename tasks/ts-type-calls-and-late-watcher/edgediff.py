#!/usr/bin/env python3
"""edgediff.py <binA> <binB> <work>: copy each corpus of /var/tmp/d446/corpora
(without .code-graph) to <work>/{A,B}/<c>, full-index it with each binary under
a sandbox HOME, then compare every edge. Report: edge set without confidence
(must be equal), and every confidence change grouped by (old, new, target type,
target language, relation)."""
import collections, os, shutil, sqlite3, subprocess, sys, tarfile, time
binA, binB, work = sys.argv[1:4]
src_root = "/var/tmp/d446/corpora"
home = os.path.join(work, "home"); os.makedirs(home, exist_ok=True)
env = dict(os.environ, HOME=home, CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
Q = """select coalesce(sf.path,'?'), sn.name, coalesce(sn.qualified_name,''), sn.type,
              coalesce(tf.path,'?'), tn.name, coalesce(tn.qualified_name,''), tn.type,
              coalesce(tf.language,'?'), e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def copy(src, dst):
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if os.path.exists(dst): shutil.rmtree(dst)
    subprocess.run(["tar", "-C", src, "--exclude=./.code-graph", "-cf", dst + ".tar", "."], check=True)
    os.makedirs(dst); subprocess.run(["tar", "-C", dst, "-xf", dst + ".tar"], check=True); os.remove(dst + ".tar")
def index(binary, root):
    t = time.time()
    r = subprocess.run([binary, "incremental-index"], cwd=root, env=env, capture_output=True, text=True, timeout=3600)
    if r.returncode: print("index failed", root, r.stderr[-1500:]); sys.exit(2)
    return time.time() - t
def dump(root):
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    rows = [tuple(r) for r in con.execute(Q)]; con.close(); return rows
total = collections.Counter()
for c in sorted(os.listdir(src_root)):
    a, b = os.path.join(work, "A", c), os.path.join(work, "B", c)
    copy(os.path.join(src_root, c), a); copy(os.path.join(src_root, c), b)
    ta, tb = index(binA, a), index(binB, b)
    ra, rb = dump(a), dump(b)
    ka = collections.Counter(r[:-1] for r in ra); kb = collections.Counter(r[:-1] for r in rb)
    same_set = ka == kb
    ca = collections.defaultdict(list); cb = collections.defaultdict(list)
    for r in ra: ca[r[:-1]].append(r[-1])
    for r in rb: cb[r[:-1]].append(r[-1])
    moves = collections.Counter()
    for k in ca:
        if k in cb and sorted(ca[k]) != sorted(cb[k]):
            for x, y in zip(sorted(ca[k]), sorted(cb[k])):
                if x != y: moves[(x, y, k[7], k[8], k[9])] += 1
    print(f"{c}: edges A={len(ra)} B={len(rb)} edge-set-equal={same_set} index-secs A={ta:.1f} B={tb:.1f}")
    if not same_set:
        only_a = ka - kb; only_b = kb - ka
        print(f"   only-A {sum(only_a.values())} only-B {sum(only_b.values())}")
        for k in list(only_a)[:5]: print("   -A", k)
        for k in list(only_b)[:5]: print("   +B", k)
    for k, n in sorted(moves.items(), key=lambda x: -x[1]):
        print(f"   {k[0]} -> {k[1]}  target={k[2]}/{k[3]} rel={k[4]}: {n}")
        total[(c,) + k] += n
