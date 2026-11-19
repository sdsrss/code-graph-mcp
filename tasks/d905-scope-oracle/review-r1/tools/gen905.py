"""D#905 review generator for fuzz_seq.py: builtin-named defs, star imports appearing/disappearing,
module variables, comprehension/lambda/with/except variables, class bodies, aliases, globals."""
NAMES = ["set", "list", "open", "helper", "Model", "T"]
DEFS = ["lib/defs.py", "lib/other.py", "lib/__init__.py"]
CALLERS = ["app/u.py", "app/v.py", "app/__init__.py", "main.py"]
ALL = DEFS + CALLERS

def def_file(rng, path):
    out = []
    for nm in NAMES:
        r = rng.random()
        if r < 0.3: out.append(f"def {nm}(*a):\n    return 1\n")
        elif r < 0.4: out.append(f"class {nm}:\n    pass\n")
        elif r < 0.5: out.append(f"{nm} = {rng.randint(0, 9)}\n")
        elif r < 0.55: out.append(f"class K{nm}:\n    def {nm}(self):\n        return {nm}()\n")
    if rng.random() < 0.2: out.insert(0, rng.choice(["from lib.defs import *\n", "from lib.other import *\n"]))
    rng.shuffle(out)
    return "\n\n".join(out) or "pass\n"

SNIPS = [
    "def f{i}(xs):\n    return {n}(xs)\n",
    "def f{i}(xs):\n    return [{n} for {n} in xs]\n",
    "def f{i}(xs):\n    return sorted(xs, key=lambda {n}: {n})\n",
    "def f{i}({n}):\n    return {n}\n",
    "def f{i}():\n    with open('x') as {n}:\n        return {n}\n",
    "def f{i}():\n    try:\n        pass\n    except Exception as {n}:\n        return {n}\n",
    "def f{i}(xs):\n    if ({n} := xs):\n        return {n}\n",
    "def f{i}():\n    {n} = 3\n    def g():\n        return {n}\n    return g\n",
    "def f{i}():\n    global {n}\n    {n} = 2\n",
    "{n} = {r}\n",
    "x{i} = {n}\n",
    "y{i} = {n}()\n",
    "@{n}\ndef d{i}():\n    pass\n",
    "def a{i}(x: {n}) -> {n}:\n    return x\n",
    "class C{i}:\n    def {n}(self):\n        return 1\n    z = {n}()\n    w = [{n} for _ in range(2)]\n",
    "class D{i}:\n    {n} = {r}\n    def m(self):\n        return {n}\n",
    "from lib.defs import {n}\n",
    "from lib.other import {n} as _{n}\n{n} = _{n}\n",
    "import lib.defs as ld\n{n} = ld.{n}\n",
    "import lib.defs as ld\n{n}, q{i} = ld.{n}, 1\n",
    "from lib.defs import *\n",
    "from lib.other import *\n",
    "cb{i} = [{n}]\nreg{i} = dict(k={n})\n",
]

def caller_file(rng, path):
    out = []
    for i in range(rng.randint(2, 6)):
        out.append(rng.choice(SNIPS).format(i=i, n=rng.choice(NAMES), r=rng.randint(0, 9)))
    # star imports first so the file stays importable-looking
    out.sort(key=lambda s: 0 if s.startswith("from") or s.startswith("import") else 1)
    return "\n\n".join(out)

def gen(rng, p):
    return def_file(rng, p) if p in DEFS else caller_file(rng, p)

def initial(rng):
    return {p: gen(rng, p) for p in ALL if rng.random() < 0.9 or p.endswith("__init__.py")}

def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 2)):
        p = rng.choice(ALL); r = rng.random()
        if p in st and r < 0.15: del st[p]
        elif p in st and r < 0.5:
            body = st[p]
            star = "from lib.defs import *\n"
            st[p] = body.replace(star, "") if star in body else star + body
        else:
            st[p] = gen(rng, p)
    return st
