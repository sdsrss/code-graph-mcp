#!/usr/bin/env python3
"""fuzz518.py <corpus-git-dir> <name> <seed> <nops>: random edits aimed at D#518's inputs
(module attributes that P.X() calls bind, decoys, re-exports, __init__.py, renames).
After each op, compare the incremental index with a fresh index of the same tree, per arm."""
import os, sys, random, shutil, subprocess, sqlite3, collections, time
R = "/var/tmp/d520-review"
corpus, name, seed, nops = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
arms = os.environ.get("ARMS", "base,head").split(",")
env = dict(os.environ, HOME=f"{R}/home", CODE_GRAPH_NO_UPDATE_CHECK="1", CODE_GRAPH_NO_AUTO_UPDATE="1")
W = f"{R}/fuzz/{name}-{seed}"
Q = """select coalesce(sf.path,'?'), sn.name, coalesce(sn.qualified_name,''), sn.start_line, coalesce(tf.path,'?'), tn.name, coalesce(tn.qualified_name,''), tn.start_line,
              e.relation, coalesce(e.metadata,''), e.confidence
       from edges e join nodes sn on sn.id=e.source_id left join files sf on sf.id=sn.file_id
                    join nodes tn on tn.id=e.target_id left join files tf on tf.id=tn.file_id"""
def sh(c): subprocess.run(c, shell=True, check=True)
def index(arm, d):
    r = subprocess.run([f"{R}/bin/cgm-{arm}", "incremental-index", "--no-embed"], cwd=d, env=env, capture_output=True, text=True)
    if r.returncode: print("INDEX FAIL", arm, d, r.stderr[-1500:], flush=True)
def dump(d):
    con = sqlite3.connect(f"file:{d}/.code-graph/index.db?mode=ro", uri=True)
    c = collections.Counter(tuple(r) for r in con.execute(Q)); con.close(); return c
if os.path.exists(W): shutil.rmtree(W)
os.makedirs(W)
src = W + "/src"; os.makedirs(src); sh(f"git -C {corpus} archive HEAD | tar -x -C {src}")
# targets from a head index of the pristine tree
probe = W + "/probe"; shutil.copytree(src, probe, symlinks=True); sh(f"git init -q {probe}"); index("head", probe)
con = sqlite3.connect(f"{probe}/.code-graph/index.db")
bound = con.execute("""select distinct tf.path, tn.name, tn.start_line, tn.end_line from edges e join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
   where e.relation='imports' and e.metadata like '%"attr":%' and tn.name<>'<module>' order by 1,2,3""").fetchall()
fallback = con.execute("""select distinct tf.path, json_extract(e.metadata,'$.attr') from edges e join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
   where e.relation='imports' and e.metadata like '%"attr":%' and tn.name='<module>' order by 1,2""").fetchall()
callers = [r[0] for r in con.execute("""select distinct sf.path from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id
   where e.relation='imports' and e.metadata like '%"attr":%' order by 1""")]
inits = [r[0] for r in con.execute("select path from files where path like '%__init__.py' order by 1")]
con.close(); shutil.rmtree(probe)
print(f"{name} seed {seed}: {len(bound)} bound targets, {len(fallback)} fallbacks, {len(callers)} callers, {len(inits)} inits", flush=True)
rng = random.Random(seed); ops = []; decoys = 0; live_decoys = []; deleted_inits = []; renamed = []
for _ in range(nops):
    k = rng.choice(["drop_def", "drop_def", "restore", "decoy", "rm_decoy", "add_def", "add_def", "touch", "del_init", "add_init", "rename"])
    if k == "drop_def" and bound: ops.append(("drop_def",) + rng.choice(bound))
    elif k == "restore" and bound: ops.append(("restore", rng.choice(bound)[0]))
    elif k == "decoy" and (bound or fallback):
        nm = rng.choice(bound)[1] if bound and (not fallback or rng.random() < .5) else rng.choice(fallback)[1]
        decoys += 1; p = f"zz_decoy_{decoys}.py"; live_decoys.append(p); ops.append(("write", p, f"def {nm}():\n    pass\n"))
    elif k == "rm_decoy" and live_decoys: ops.append(("delete", live_decoys.pop(rng.randrange(len(live_decoys)))))
    elif k == "add_def" and fallback:
        p, nm = rng.choice(fallback); ops.append(("append", p, f"\n\ndef {nm}():\n    pass\n"))
    elif k == "touch" and callers: ops.append(("append", rng.choice(callers), "\n# touched\n"))
    elif k == "del_init" and inits:
        f = rng.choice(inits); ops.append(("delete", f)); deleted_inits.append(f)
    elif k == "add_init" and deleted_inits: ops.append(("restore", deleted_inits.pop()))
    elif k == "rename" and bound:
        f = rng.choice(bound)[0]
        if not f.endswith("__init__.py") and f not in renamed:
            renamed.append(f); ops.append(("rename", f, f[:-3] + "_renamed.py"))
bad = collections.Counter()
if os.environ.get("OPS"):
    for i,o in enumerate(ops): print("   op",i,o[:3] if o[0]!="write" else o[:2])
for arm in arms:
    inc = f"{W}/{arm}/inc"; shutil.copytree(src, inc, symlinks=True); sh(f"git init -q {inc}"); index(arm, inc)
    prev = (collections.Counter(), collections.Counter())
    for i, op in enumerate(ops):
        p = os.path.join(inc, op[1])
        if op[0] == "drop_def" and os.path.exists(p):
            lines = open(p).read().split("\n")
            if op[3] - 1 < len(lines):
                for j in range(op[3] - 1, min(op[4], len(lines))): lines[j] = ""
                open(p, "w").write("\n".join(lines))
        elif op[0] == "drop_def": pass
        elif op[0] == "restore": os.makedirs(os.path.dirname(p) or inc, exist_ok=True); shutil.copy(os.path.join(src, op[1]), p)
        elif op[0] == "write": open(p, "w").write(op[2])
        elif op[0] == "append":
            if os.path.exists(p): open(p, "a").write(op[2])
        elif op[0] == "delete":
            if os.path.exists(p): os.remove(p)
        elif op[0] == "rename":
            if os.path.exists(p): os.rename(p, os.path.join(inc, op[2]))
        t = time.time() + 2
        if os.path.exists(p): os.utime(p, (t, t))
        index(arm, inc)
        fr = f"{W}/{arm}/fresh"
        if os.path.exists(fr): shutil.rmtree(fr)
        shutil.copytree(inc, fr, symlinks=True, ignore=shutil.ignore_patterns(".code-graph", ".git")); sh(f"git init -q {fr}"); index(arm, fr)
        a, f = dump(inc), dump(fr); oa, of = a - f, f - a
        if oa or of:
            bad[arm] += 1
            print(f"  [{arm}] op{i} {op[:3] if op[0] != 'write' else op[:2]}: +inc={sum(oa.values())} +fresh={sum(of.values())}", flush=True)
            if os.environ.get("V"):
                for r in sorted(oa)[:6]: print("      +inc  ", r)
                for r in sorted(of)[:6]: print("      +fresh", r)
print(f"{name} seed {seed}: {len(ops)} ops, divergent steps per arm: {dict(bad)}", flush=True)
