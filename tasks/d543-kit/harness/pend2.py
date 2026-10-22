import sys, os, sqlite3
sys.path.insert(0, "/var/tmp/d543/harness")
from corp import *
binp, rel, name = sys.argv[1], sys.argv[2], sys.argv[3]
w = f"{R}/work/pend2"; inc = w + "/inc"
def pend(root, label):
    con = sqlite3.connect(f"file:{root}/.code-graph/index.db?mode=ro", uri=True)
    rows = con.execute("""select f.path, count(*), group_concat(distinct coalesce(p.metadata,'-')), max(p.attempts) from pending_unresolved_calls p
        join nodes s on s.id=p.source_id join files f on f.id=s.file_id where p.target_name=? group by 1 order by 2 desc""", (name,)).fetchall()
    print(f"  {label}:"); [print("    ", r) for r in rows[:14]]
    con.close()
copy(f"{R}/corpus/repo", inc); index(binp, inc)
p = os.path.join(inc, rel); keep = open(p, "rb").read(); os.remove(p); index(binp, inc); pend(inc, "inc-after-delete")
