"""fuzz2.py OUT SEED NFILES | check OUT: like fuzz_mro.py with noisy class bodies and module
statements (assignments, decorated / repeated / conditional defs, patches, keyword bases,
decorated classes, nested classes, deletes). Python's lookup is the truth; every `own` edge
must name exactly the class whose __dict__ holds the attribute Python finds, and that
attribute must be the plain function the class body's `def` made."""
import importlib, json, os, random, sqlite3, sys

METHODS = ["m0", "m1", "m2"]


def deco_src():
    return "def ident(x):\n    return x\n\n\ndef other_deco(f):\n    return lambda *a, **k: 'wrapped'\n\n"


def gen_lib(rng):
    out = []
    names = []
    for i in range(4):
        bases = [b for b in names if rng.random() < 0.4]
        out.append(f"class L{i}({', '.join(bases)}):" if bases else f"class L{i}:")
        body = [f"    def {m}(self):\n        return 'lib.L{i}.{m}'" for m in METHODS if rng.random() < 0.3]
        out.append("\n".join(body) if body else "    pass")
        out.append("")
        names.append(f"L{i}")
    return "\n".join(out) + "\n"


def body_stmt(rng, cls, m, i):
    r = rng.random()
    if r < 0.45:
        return f"    def {m}(self):\n        return '{cls}.{m}'"
    if r < 0.52:
        return f"    @ident\n    def {m}(self):\n        return '{cls}.{m}'"
    if r < 0.57:
        return f"    @other_deco\n    def {m}(self):\n        return '{cls}.{m}'"
    if r < 0.62:
        return f"    {m} = lambda self: '{cls}.{m}.lambda'"
    if r < 0.66:
        return f"    def {m}(self):\n        return '{cls}.{m}.first'\n    def {m}(self):\n        return '{cls}.{m}'"
    if r < 0.70:
        return f"    if True:\n        def {m}(self):\n            return '{cls}.{m}.cond'"
    if r < 0.73:
        return f"    def {m}(self):\n        return '{cls}.{m}'\n    {m}.tag = 1"
    if r < 0.76:
        return f"    class {m}:\n        pass"
    if r < 0.79:
        return f"    def {m}(self):\n        return '{cls}.{m}'\n    del {m}"
    if r < 0.82:
        return f"    def helper{i}(self):\n        def {m}():\n            return 'inner'\n        return {m}"
    return None


def gen_file(rng, idx):
    out = ["from lib import L0, L1, L2, L3", "", deco_src()]
    names = []
    post = []
    n = rng.randint(3, 8)
    for i in range(n):
        name = f"K{i}"
        pool = names + [f"L{j}" for j in range(4) if rng.random() < 0.2]
        bases = [b for b in pool if rng.random() < 0.3]
        rng.shuffle(bases)
        if rng.random() < 0.08:
            bases.append("metaclass=type")
        head = f"class {name}({', '.join(bases)}):" if bases else f"class {name}:"
        if rng.random() < 0.06:
            head = "@ident\n" + head
        body = []
        for j, m in enumerate(METHODS):
            s = body_stmt(rng, name, m, j) if rng.random() < 0.5 else None
            if s:
                body.append(s)
        out.append(head)
        out.append("\n".join(body) if body else "    pass")
        out.append("")
        names.append(name)
        r = rng.random()
        if r < 0.05:
            post.append(f"{name}.{rng.choice(METHODS)} = lambda self: 'patched'")
        elif r < 0.08:
            post.append(f"{name} = {name}")
        elif r < 0.10:
            post.append(f"if False:\n    class {name}:\n        pass")
    out += post
    out.append("")
    for k in names:
        for m in METHODS:
            out.append(f"def t_{k}_{m}():\n    return {k}.{m}(None)\n")
    return "\n".join(out), names


def truth(mod, k, m):
    obj = getattr(mod, k)
    if not isinstance(obj, type):
        return None
    for c in type(obj).__mro__:
        d = c.__dict__.get(m)
        if d is not None and hasattr(d, "__set__"):
            return "META"
    for c in obj.__mro__:
        if m in c.__dict__:
            f = c.__dict__[m]
            if getattr(f, "__qualname__", "").startswith("other_deco."):
                return f"{c.__module__}.{c.__name__}"
            if callable(f) and getattr(f, "__name__", "") == m and getattr(f, "__qualname__", "") == f"{c.__name__}.{m}":
                return f"{c.__module__}.{c.__name__}"
            return f"NONDEF:{c.__module__}.{c.__name__}"
    return None


def write(out, seed, nfiles):
    rng = random.Random(seed)
    os.makedirs(out, exist_ok=True)
    sys.path.insert(0, out)
    sys.dont_write_bytecode = True
    while True:
        open(os.path.join(out, "lib.py"), "w").write(gen_lib(rng))
        importlib.invalidate_caches()
        sys.modules.pop("lib", None)
        try:
            importlib.import_module("lib")
            break
        except TypeError:
            continue
    ns = []
    for i in range(8):
        ns.append(f"class K{i}:\n" + "\n".join(f"    def {m}(self):\n        return 'b.K{i}'" for m in METHODS) + "\n")
    open(os.path.join(out, "b.py"), "w").write("\n".join(ns))
    made, expect = 0, {}
    while made < nfiles:
        text, names = gen_file(rng, made)
        open(os.path.join(out, f"a{made}.py"), "w").write(text)
        importlib.invalidate_caches()
        try:
            mod = importlib.import_module(f"a{made}")
        except TypeError:
            continue
        for k in names:
            for m in METHODS:
                expect[f"a{made}.py:t_{k}_{m}"] = truth(mod, k, m)
        made += 1
    json.dump(expect, open(out + ".expect.json", "w"))


def check(out):
    expect = json.load(open(out + ".expect.json"))
    con = sqlite3.connect(f"file:{out}/.code-graph/index.db?mode=ro", uri=True)
    got = {}
    for sp, sn, tp, tq, tl, meta in con.execute(
            """select sf.path, sn.name, tf.path, tn.qualified_name, tn.start_line, coalesce(e.metadata,'')
               from edges e join nodes sn on sn.id=e.source_id join files sf on sf.id=sn.file_id
               join nodes tn on tn.id=e.target_id join files tf on tf.id=tn.file_id
               where e.relation='calls' and sn.name like 't\\_%' escape '\\'"""):
        got.setdefault(f"{sp}:{sn}", []).append((tp, tq, tl, meta))
    st = {"own": 0, "right": 0, "wrong": 0, "declined": 0}
    bad = []
    for key, want in sorted(expect.items()):
        own = [r for r in got.get(key, []) if '"own"' in r[3]]
        if not own:
            st["declined"] += 1
            continue
        st["own"] += 1
        if len(own) > 1:
            st["multi"] = st.get("multi", 0) + 1
        names = sorted(set(f"{os.path.splitext(tp)[0]}.{tq.rsplit('.', 1)[0]}" for tp, tq, _, _ in own))
        if names == [want]:
            st["right"] += 1
        else:
            st["wrong"] += 1
            bad.append((key, want, names, [(r[2], r[3]) for r in own]))
    print(json.dumps(st))
    for b in bad[:40]:
        print("WRONG", b)


if __name__ == "__main__":
    if sys.argv[1] == "check":
        check(sys.argv[2])
    else:
        write(sys.argv[1], int(sys.argv[2]), int(sys.argv[3]))
