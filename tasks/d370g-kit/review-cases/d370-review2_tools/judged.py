import json, sqlite3, sys, collections
# judged.py ARM_DIR JUDGED_BASE JUDGED_HEAD: verdicts of own-marked edges, and of pairs base judged that head lacks
arm, jb, jh = sys.argv[1:4]
def load(p):
    return {(r["caller_at"], r["callee_at"]): r for r in json.load(open(p))}
B, H = load(jb), load(jh)
con = sqlite3.connect(f"file:{arm}/.code-graph/index.db?mode=ro", uri=True)
own = set()
for sp, sl, tp, tl in con.execute("""select sf.path, sn.start_line, tf.path, tn.start_line from edges e join nodes sn on sn.id=e.source_id
     join files sf on sf.id=sn.file_id join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
     where e.relation='calls' and e.metadata like '%"own"%'"""):
    own.add((f"{sp}:{sl}", f"{tp}:{tl}"))
c = collections.Counter()
for k in own:
    r = H.get(k); c[(r["tier"], r["verdict"]) if r else "unjudged"] += 1
print("own edges", len(own), dict(c))
for k in sorted(own):
    r = H.get(k)
    if r and r["verdict"] != "correct": print("  OWN-NOT-CORRECT", k, r)
gone = [k for k in B if k not in H]
c = collections.Counter((B[k]["tier"], B[k]["verdict"]) for k in gone)
print("pairs judged in base, absent in head:", len(gone), dict(c))
for k in gone:
    if B[k]["verdict"] == "correct": print("  LOST", B[k])
new = [k for k in H if k not in B]
print("pairs judged in head, absent in base:", len(new), collections.Counter((H[k]["tier"], H[k]["verdict"]) for k in new))
moved = [k for k in H if k in B and (B[k]["tier"], B[k]["verdict"]) != (H[k]["tier"], H[k]["verdict"])]
for k in moved: print("  MOVED", B[k]["tier"], B[k]["verdict"], "->", H[k]["tier"], H[k]["verdict"], k)
