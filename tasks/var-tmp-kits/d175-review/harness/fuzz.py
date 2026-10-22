"""fuzz.py <corpus> <seed> <nops> [arms]: random single-file ops; after each, compare
incremental with a fresh index of the same tree."""
import sys, os, random, collections, shutil
sys.path.insert(0, "/var/tmp/d175-review/harness")
from corp import *
EXT = os.environ.get("EXT", ".py")
corpus, seed, nops = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
arms = sys.argv[4].split(",") if len(sys.argv) > 4 else ["head", "base"]
BIN = {"head": f"{R}/bin/cgm-head", "base": f"{R}/bin/cgm-base", "b41": f"{R}/bin/cgm-b41"}
def pyfiles(root):
    out = []
    for d, ds, fs in os.walk(root):
        ds[:] = [x for x in ds if x not in (".git", ".code-graph", "__pycache__")]
        for f in fs:
            if f.endswith(EXT) and not os.path.islink(os.path.join(d, f)): out.append(os.path.relpath(os.path.join(d, f), root))
    return sorted(out)
def plan(root, rng, n):
    ops = []; files = pyfiles(root); deleted = []
    for _ in range(n):
        kind = rng.choice(["touch", "touch", "delete", "restore", "add_init", "del_init", "rename"] if EXT == ".py" else ["touch", "touch", "delete", "restore", "rename"])
        if kind == "restore" and not deleted: kind = "touch"
        if kind == "touch": ops.append(("touch", rng.choice(files)))
        elif kind == "delete":
            f = rng.choice([x for x in files if not x.endswith("__init__.py")]); files.remove(f); deleted.append(f); ops.append(("delete", f))
        elif kind == "restore":
            f = deleted.pop(); files.append(f); ops.append(("restore", f))
        elif kind == "add_init":
            dirs = sorted({os.path.dirname(f) for f in files if os.path.dirname(f)} - {os.path.dirname(f) for f in files if f.endswith("__init__.py")})
            if not dirs: continue
            d = rng.choice(dirs); f = os.path.join(d, "__init__.py"); files.append(f); ops.append(("add_init", f))
        elif kind == "del_init":
            inits = [f for f in files if f.endswith("__init__.py")]
            if not inits: continue
            f = rng.choice(inits); files.remove(f); deleted.append(f); ops.append(("delete", f))
        elif kind == "rename":
            f = rng.choice([x for x in files if not x.endswith("__init__.py")]); g = f[:-len(EXT)] + "_renamed" + EXT
            files.remove(f); files.append(g); ops.append(("rename", f, g))
    return ops
rng = random.Random(seed)
ops = None
src_copy = f"{R}/work/fuzz/src"
copy(corpus, src_copy)
ops = plan(src_copy, rng, nops)
for a in arms:
    w = f"{R}/work/fuzz/{a}"; inc = w + "/inc"
    copy(corpus, inc); index(BIN[a], inc)
    bad = 0; prev = (collections.Counter(), collections.Counter())
    for i, op in enumerate(ops):
        p = os.path.join(inc, op[1])
        if os.path.exists(p): os.chmod(p, 0o644)
        if op[0] == "touch": open(p, "a").write(f"\n# fuzz {i}\n")
        elif op[0] == "delete": os.remove(p)
        elif op[0] == "restore": shutil.copy(os.path.join(src_copy, op[1]), p)
        elif op[0] == "add_init": open(p, "w").write("")
        elif op[0] == "rename": os.rename(p, os.path.join(inc, op[2]))
        index(BIN[a], inc)
        fr = w + "/fresh"; copy(inc, fr); index(BIN[a], fr)
        a_, f_ = dump(inc), dump(fr)
        oa, of = a_ - f_, f_ - a_
        newa, newf = oa - prev[0], of - prev[1]
        gone = (prev[0] - oa) + (prev[1] - of)
        print(f"   [{a}] op{i} {op} total only-inc={sum(oa.values())} only-fresh={sum(of.values())} new: +inc {sum(newa.values())} +fresh {sum(newf.values())} healed {sum(gone.values())}")
        for k in sorted(newa)[:60]: print("       +inc  ", fmt(k))
        for k in sorted(newf)[:60]: print("       +fresh", fmt(k))
        prev = (oa, of)
        bad += bool(newa or newf)
    print(f"[{a}] ops with divergence: {bad}/{len(ops)}")
