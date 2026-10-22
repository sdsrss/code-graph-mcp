import sys, os, sqlite3, subprocess
sys.path.insert(0, "/var/tmp/d543/harness")
from corp import *
binp, rel = sys.argv[1], sys.argv[2]
w = f"{R}/work/probe3"; inc = w + "/inc"
def ids(root):
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    r = dict(con.execute("select f.path, min(n.id) from nodes n join files f on f.id=n.file_id group by 1").fetchall()); con.close(); return r
copy(f"{R}/corpus/repo", inc); index(binp, inc); a = ids(inc)
p = os.path.join(inc, rel); keep = open(p, "rb").read(); os.remove(p)
e = dict(env, RUST_LOG="info")
r = subprocess.run([binp, "incremental-index", "--no-embed"], cwd=inc, env=e, capture_output=True, text=True)
print("\n".join(l for l in r.stderr.splitlines() if "Phase" in l or "re-extract" in l.lower() or "dependent" in l.lower())[:3000])
b = ids(inc)
print("re-extracted on delete:", sorted(k for k in b if a.get(k) != b[k]))
open(p, "wb").write(keep)
r = subprocess.run([binp, "incremental-index", "--no-embed"], cwd=inc, env=e, capture_output=True, text=True)
print("\n".join(l for l in r.stderr.splitlines() if "Phase" in l or "re-extract" in l.lower() or "dependent" in l.lower())[:3000])
c = ids(inc)
print("re-extracted on restore:", sorted(k for k in c if b.get(k) != c[k]))
