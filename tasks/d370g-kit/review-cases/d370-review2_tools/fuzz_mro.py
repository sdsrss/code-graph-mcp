"""fuzz_mro.py OUT SEED NFILES: random class hierarchies; Python's own MRO lookup vs the
owner an indexer build stamps on `K.m()` calls (`own` edges). Writes the project to OUT,
then `check` mode compares after indexing:  fuzz_mro.py check OUT"""
import importlib, json, os, random, sqlite3, sys

METHODS = ["m0", "m1", "m2", "m3"]


def gen_lib(rng):
    lines = []
    names = []
    for i in range(5):
        bases = [b for b in names if rng.random() < 0.35]
        rng.shuffle(bases)
        lines.append(f"class L{i}({', '.join(bases)}):" if bases else f"class L{i}:")
        body = [f"    def {m}(self):\n        return 'lib.L{i}'" for m in METHODS if rng.random() < 0.3]
        lines.append("\n".join(body) if body else "    pass")
        lines.append("")
        names.append(f"L{i}")
    return "\n".join(lines) + "\n"


def gen_file(rng, idx):
    out = ["from lib import L0, L1, L2, L3, L4", ""]
    names = []
    n = rng.randint(3, 9)
    for i in range(n):
        pool = names + [f"L{j}" for j in range(5) if rng.random() < 0.15]
        bases = [b for b in pool if rng.random() < 0.3]
        rng.shuffle(bases)
        # repeated base sometimes (Python refuses, file is then regenerated)
        out.append(f"class K{i}({', '.join(bases)}):" if bases else f"class K{i}:")
        body = [f"    def {m}(self):\n        return 'a{idx}.K{i}'" for m in METHODS if rng.random() < 0.3]
        out.append("\n".join(body) if body else "    pass")
        out.append("")
        names.append(f"K{i}")
    for k in names:
        for m in METHODS:
            out.append(f"def t_{k}_{m}():\n    return {k}.{m}(None)\n")
    return "\n".join(out), names


def truth(mod, k, m):
    cls = getattr(mod, k)
    for c in cls.__mro__:
        if m in c.__dict__:
            return f"{c.__module__}.{c.__name__}"
    return None


def write(out, seed, nfiles):
    rng = random.Random(seed)
    os.makedirs(out, exist_ok=True)
    sys.path.insert(0, out)
    sys.dont_write_bytecode = True
    open(os.path.join(out, "lib.py"), "w").write(gen_lib(rng))
    # namesakes of every K in another file, each defining every method
    ns = []
    for i in range(10):
        ns.append(f"class K{i}:\n" + "\n".join(f"    def {m}(self):\n        return 'b.K{i}'" for m in METHODS) + "\n")
    open(os.path.join(out, "b.py"), "w").write("\n".join(ns))
    made = 0
    expect = {}
    while made < nfiles:
        text, names = gen_file(rng, made)
        p = os.path.join(out, f"a{made}.py")
        open(p, "w").write(text)
        importlib.invalidate_caches()
        try:
            mod = importlib.import_module(f"a{made}")
        except TypeError:
            continue
        for k in names:
            for m in METHODS:
                expect[f"a{made}.py:t_{k}_{m}"] = truth(mod, k, m)
        made += 1
    json.dump(expect, open(os.path.join(out, "..", os.path.basename(out) + ".expect.json"), "w"))


def check(out):
    expect = json.load(open(os.path.join(out, "..", os.path.basename(out) + ".expect.json")))
    con = sqlite3.connect(f"file:{out}/.code-graph/index.db?mode=ro", uri=True)
    q = """select sf.path, sn.name, tf.path, tn.qualified_name, coalesce(e.metadata,'')
           from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id
           join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
           where e.relation='calls' and sn.name like 't\\_%' escape '\\'"""
    got = {}
    for sp, sn, tp, tq, meta in con.execute(q):
        got.setdefault(f"{sp}:{sn}", []).append((tp, tq, meta))
    stats = {"own": 0, "own_right": 0, "own_wrong": 0, "declined": 0, "declined_decidable": 0}
    bad = []
    for key, want in sorted(expect.items()):
        rows = got.get(key, [])
        own = [r for r in rows if '"own"' in r[2]]
        if own:
            stats["own"] += 1
            mod = key.split(".py:")[0]
            names = sorted(f"{os.path.splitext(tp)[0]}.{tq.rsplit('.', 1)[0]}" for tp, tq, _ in own)
            if names == [want]:
                stats["own_right"] += 1
            else:
                stats["own_wrong"] += 1
                bad.append((key, want, names, [r[2] for r in own]))
        else:
            stats["declined"] += 1
            if want and want.startswith(key.split(".py:")[0] + "."):
                stats["declined_decidable"] += 1
    print(json.dumps(stats))
    for b in bad[:30]:
        print("WRONG", b)


if __name__ == "__main__":
    if sys.argv[1] == "check":
        check(sys.argv[2])
    else:
        write(sys.argv[1], int(sys.argv[2]), int(sys.argv[3]))
