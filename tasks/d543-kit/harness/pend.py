"""pend.py <bin> <relpath> <name>...: pending rows for <name> after delete (inc and fresh) and after restore."""
import sys, os, sqlite3
sys.path.insert(0, "/var/tmp/d543/harness")
from corp import *
binp, rel, names = sys.argv[1], sys.argv[2], sys.argv[3:]
w = f"{R}/work/pend"; inc = w + "/inc"; fr = w + "/fresh"
def pend(root, label):
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    for n in names:
        rows = con.execute("""select f.path, s.name, p.metadata, p.attempts from pending_unresolved_calls p
            join nodes s on s.id=p.source_id join files f on f.id=s.file_id where p.target_name=?""", (n,)).fetchall()
        print(f"  {label} pending {n}: {len(rows)}", rows[:3])
    con.close()
copy(f"{R}/corpus/repo", inc); index(binp, inc); pend(inc, "initial")
p = os.path.join(inc, rel); keep = open(p, "rb").read(); os.remove(p); index(binp, inc); pend(inc, "inc-after-delete")
copy(inc, fr); index(binp, fr); pend(fr, "fresh-of-deleted")
open(p, "wb").write(keep); index(binp, inc); pend(inc, "inc-after-restore")
