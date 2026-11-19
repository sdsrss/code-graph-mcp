import sys, os, sqlite3
sys.path.insert(0, os.path.dirname(__file__))
from reforacle import Corpus, classify, Q
from collections import Counter
root = os.path.abspath(sys.argv[1]); rel = sys.argv[2]
con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
files = [r[0] for r in con.execute("SELECT path FROM files WHERE language='python'")]
corpus = Corpus(root, files)
t = Counter()
for row in con.execute(Q, (rel,)):
    spath, stype, sname, s0, s1, tpath, tname, tqn, ttype, conf, t0, t1 = row
    v, info = classify(corpus, spath, stype, sname, s0, s1, tpath, tname, tqn, ttype, t0, t1, rel)
    if v in ("unmatched", "parse-fail"): continue
    vis = conf != "ambiguous"
    t[(v, "method" if ttype == "method" else "nonmethod", vis)] += 1
print(os.path.basename(root), rel)
for v in ["ok","unknown","wrong-other","wrong-var","wrong-ext","wrong-builtin","wrong-local"]:
    row = [t[(v,k,vis)] for k in ("method","nonmethod") for vis in (True, False)]
    if sum(row): print(f"  {v:14} method vis {row[0]:5} amb {row[1]:5} | nonmethod vis {row[2]:5} amb {row[3]:5}")
