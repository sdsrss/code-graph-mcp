#!/usr/bin/env python3
"""A second state generator for fuzz_seq.py (`--gen gen_review.py`), written
by the 0.200.0 pre-tag review (R1): classes and methods, member / `self.x` /
module-attribute calls, star imports, `a = _a`, try/except, conditional,
function-level and class-level imports, decorators, bare references, package
`__init__` edits. Exposes `initial(rng)` and `step(state, rng)`.
"""
NAMES = ["a", "b", "c"]
DEFS = ["app/m.py", "app/n.py", "lib/k.py", "app/sub/impl.py", "tests/helpers.py", "other.py"]
CALLERS = ["app/views.py", "app/rev.py", "use.py", "lib/user.py", "tests/test_x.py"]
INITS = {"app/__init__.py": ["", "from .m import a\n", "from .n import a as b\n", "from .m import *\n", "from .sub import a\n"],
         "app/sub/__init__.py": ["", "from .impl import a\n", "from .impl import a as b\n"],
         "lib/__init__.py": ["", None], "tests/__init__.py": ["", None]}
MODS = {"app/m.py": "app.m", "app/n.py": "app.n", "lib/k.py": "lib.k", "app/sub/impl.py": "app.sub.impl",
        "tests/helpers.py": "tests.helpers", "other.py": "other"}

def def_file(rng, path):
    out = []
    for nm in NAMES:
        r = rng.random()
        if r < 0.4: out.append(f"def {nm}():\n    return {rng.randint(0, 9)}\n")
        elif r < 0.47: out.append(f"{nm} = {rng.randint(0, 9)}\n")
        elif r < 0.52: out.append(f"def {nm}2():\n    return 0\n")
    if rng.random() < 0.35: out.append("class A:\n    def a(self):\n        return 1\n\n    def run(self):\n        return self.a()\n")
    if rng.random() < 0.15: out.append("class B(A):\n    def b(self):\n        return a()\n" if "class A" in "".join(out) else "class B:\n    pass\n")
    if rng.random() < 0.2:
        src = rng.choice([m for p, m in MODS.items() if p != path])
        out.insert(0, rng.choice([f"from {src} import a as b\n", f"from {src} import a\n", f"from {src} import *\n"]))
    rng.shuffle(out)
    return "\n\n".join(out)

def imports_for(rng, path):
    mods = list(MODS.values())
    pool = []
    for _ in range(6):
        m = rng.choice(mods); x = rng.choice(["a", "A", "b"]); y = rng.choice(["b", "c", "B", "_x", x])
        pool += [f"from {m} import {x} as {y}", f"from {m} import {x}"]
    pool += [f"from {rng.choice(mods)} import *", "import app.m as mm", "from app import m as mm", "from app import m",
             "import app.m", "from app import a", "from app import b", "from app.sub import a as b"]
    if path.startswith("app/"):
        pool += ["from .m import a as b", "from .n import a", "from . import m", "from .sub.impl import a as c"]
    return [rng.choice(pool) for _ in range(rng.randint(0, 5))]

def caller_file(rng, path):
    lines = imports_for(rng, path)
    r = rng.random()
    if r < 0.12:
        lines.insert(0, f"try:\n    from {rng.choice(list(MODS.values()))} import a\nexcept ImportError:\n    from {rng.choice(list(MODS.values()))} import a as _a\n    a = _a")
    elif r < 0.2:
        lines.append(f"from {rng.choice(list(MODS.values()))} import a as _a\na = _a")
    elif r < 0.26:
        lines.insert(0, f"if True:\n    from {rng.choice(list(MODS.values()))} import a as b")
    body = []
    for nm in NAMES:
        if rng.random() < 0.5: body.append(f"def go_{nm}():\n    return {nm}()\n")
    if rng.random() < 0.3: body.append("def go_A():\n    return A()\n")
    if rng.random() < 0.2: body.append("def go_B():\n    return B()\n")
    if rng.random() < 0.2: body.append("def go_o(o):\n    return o.a()\n")
    if rng.random() < 0.2: body.append("class V:\n    def go(self):\n        return self.x.a()\n")
    if rng.random() < 0.2: body.append("def go_m():\n    return m.a()\n")
    if rng.random() < 0.15: body.append("def go_mm():\n    return mm.a()\n")
    if rng.random() < 0.15: body.append("class W(B):\n    def go(self):\n        return self.a()\n")
    if rng.random() < 0.15: body.append("@a\ndef deco():\n    return 1\n")
    if rng.random() < 0.15: body.append("REFS = [a, b]\n")
    if rng.random() < 0.15:
        src = rng.choice(list(MODS.values())); al = rng.choice(["b", "c", "a"])
        body.append(f"def go_local():\n    from {src} import a as {al}\n    return {al}() + a()\n")
    if rng.random() < 0.1: body.append(f"class K:\n    from {rng.choice(list(MODS.values()))} import a as b\n    x = b()\n")
    if rng.random() < 0.1: body.append("def go_g():\n    global a\n    return a()\n")
    if rng.random() < 0.15: body.append(f"{rng.choice(NAMES)}()\n")
    return "\n".join(lines) + "\n\n\n" + "\n\n".join(body)

def gen(rng, p):
    if p in INITS: return rng.choice(INITS[p])
    if p in DEFS: return def_file(rng, p)
    return caller_file(rng, p)

ALL = list(INITS) + DEFS + CALLERS

def initial(rng):
    st = {}
    for p in ALL:
        if p in INITS or rng.random() < 0.85:
            b = gen(rng, p)
            if b is not None: st[p] = b
    return st

def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 3)):
        p = rng.choice(ALL); r = rng.random()
        if p in st and r < 0.2: del st[p]
        elif p in st and p in DEFS and r < 0.45:
            nm = rng.choice(NAMES)
            body = st[p]
            st[p] = body.replace(f"def {nm}(", f"def {nm}2(") if f"def {nm}(" in body else body + f"\n\ndef {nm}():\n    return 7\n"
        else:
            b = gen(rng, p)
            if b is None: st.pop(p, None)
            else: st[p] = b
    return st
