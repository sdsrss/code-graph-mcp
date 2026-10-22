# Corpus-scale steps aimed at D#518's inputs, for pair.py (head vs headG) and par.py.
# CORPORA env: comma list of name=git-dir=index-db; SEED env.
import os, random, sqlite3, subprocess
S = {}
spec = os.environ.get("CORPORA", "nx=/var/tmp/d520/corpora/nx=/var/tmp/d518r2/idx/nx-head/.code-graph/index.db")
NOPS = int(os.environ.get("NOPS", "20"))
for item in spec.split(","):
    name, git, db = item.split("=")
    files = subprocess.run(["git", "-C", git, "ls-files"], capture_output=True, text=True).stdout.split()
    tree = {}
    for f in files:
        if not f.endswith(".py"): continue
        try: tree[f] = open(os.path.join(git, f), encoding="utf-8").read()
        except Exception: pass
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    bound = con.execute("""select distinct tf.path, tn.name, tn.start_line, tn.end_line from edges e join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
       where e.relation='imports' and e.metadata like '%"attr":%' and tn.name<>'<module>' order by 1,2,3""").fetchall()
    fallback = con.execute("""select distinct tf.path, json_extract(e.metadata,'$.attr') from edges e join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
       where e.relation='imports' and e.metadata like '%"attr":%' and tn.name='<module>' order by 1,2""").fetchall()
    callers = [r[0] for r in con.execute("""select distinct sf.path from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id
       where e.relation='imports' and e.metadata like '%"attr":%' order by 1""")]
    con.close()
    for seed in [int(x) for x in os.environ.get("SEEDS", "1").split(",")]:
        rng = random.Random(seed); cur = dict(tree); steps = [dict(tree)]; nd = 0
        for _ in range(NOPS):
            k = rng.choice(["drop_def", "drop_def", "decoy", "add_def", "add_def", "touch", "touch2", "rm_init"])
            st = {}
            if k == "drop_def" and bound:
                p, nm, a, b = rng.choice(bound)
                if p in cur:
                    lines = cur[p].split("\n")
                    for j in range(a - 1, min(b, len(lines))): lines[j] = ""
                    st[p] = "\n".join(lines)
            elif k == "decoy" and (bound or fallback):
                nm = rng.choice(bound)[1] if bound and (not fallback or rng.random() < .5) else rng.choice(fallback)[1]
                nd += 1; st[f"zz_decoy_{nd}.py"] = f"def {nm}():\n    pass\n"
            elif k == "add_def" and fallback:
                p, nm = rng.choice(fallback)
                if p in cur: st[p] = cur[p] + f"\n\ndef {nm}():\n    pass\n"
            elif k == "touch" and callers:
                p = rng.choice(callers)
                if p in cur: st[p] = cur[p] + "\n# touched\n"
            elif k == "touch2" and bound:
                p = rng.choice(bound)[0]
                if p in cur: st[p] = cur[p] + "\n# touched\n"
            elif k == "rm_init":
                inits = [p for p in cur if p.endswith("__init__.py")]
                if inits:
                    p = rng.choice(inits)
                    st[p] = None
            if not st: continue
            for p, c in st.items():
                if c is None: cur.pop(p, None)
                else: cur[p] = c
            steps.append(st)
        S[f"{name}_s{seed}"] = steps
SCENARIOS = S
