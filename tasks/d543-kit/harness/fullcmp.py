"""fullcmp.py <corpus> <armA> <armB>: full index of the same tree by two binaries; compare edges, nodes, context strings, pending rows."""
import sys, os, sqlite3, collections, time
sys.path.insert(0, "/var/tmp/d543/harness")
from corp import *
corpus, A, B = sys.argv[1], sys.argv[2], sys.argv[3]
name = os.path.basename(corpus.rstrip("/"))
QN = "select f.path, coalesce(n.qualified_name,n.name), n.type, n.start_line, n.end_line from nodes n join files f on f.id=n.file_id"
QC = "select f.path, coalesce(n.qualified_name,n.name), n.type, n.start_line, coalesce(n.context_string,'<null>') from nodes n join files f on f.id=n.file_id"
QP = "select f.path, s.name, s.start_line, p.target_name, coalesce(p.metadata,'') from pending_unresolved_calls p join nodes s on s.id=p.source_id join files f on f.id=s.file_id"
out = {}
for arm in (A, B):
    w = f"{R}/work/full/{name}/{arm}"; copy(corpus, w)
    if not os.path.isdir(w + "/.git"):
        subprocess.run(["git", "init", "-q", w], check=True)
    t, cpu, r = index(f"{R}/bin/cgm-{arm}", w)
    con = sqlite3.connect(f"file:{w}/.code-graph/index.db?mode=ro", uri=True)
    out[arm] = dict(e=dump(w), n=collections.Counter(con.execute(QN)), c=collections.Counter(con.execute(QC)), p=collections.Counter(con.execute(QP)),
                    meta=con.execute("select value from meta where key='vanished_file_dependents'").fetchone(), t=t, cpu=cpu)
    con.close()
a, b = out[A], out[B]
print(f"{name}: edges {sum(a['e'].values())}/{sum(b['e'].values())} diff {sum((a['e']-b['e']).values())}+{sum((b['e']-a['e']).values())}; "
      f"nodes diff {sum((a['n']-b['n']).values())}+{sum((b['n']-a['n']).values())}; ctx diff {sum((a['c']-b['c']).values())}+{sum((b['c']-a['c']).values())}; "
      f"pending {sum(a['p'].values())}/{sum(b['p'].values())} diff {sum((a['p']-b['p']).values())}+{sum((b['p']-a['p']).values())}; "
      f"vanished meta {a['meta']}/{b['meta']}; wall {a['t']:.1f}/{b['t']:.1f}s cpu {a['cpu']:.1f}/{b['cpu']:.1f}s")
for k in list((b['c'] - a['c']))[:3]: print("   ctx+B", k[:4], repr(k[4][:200]))
