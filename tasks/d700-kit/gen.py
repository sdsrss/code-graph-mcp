"""gen.py OUT N SEED: one-call Python files for D#700(2b), checked by check.py.

About a third of the cases sit inside package `pkg2/` and import a name that
may reach the case's own module (`import pkg2` + `pkg2.<self>.go()`, `from pkg2
import <self> as R`, ...) or a sibling (`pkg2.other`), as review round 1 asked:
a root-level case file never reaches itself through an import.

Each case file `cNNNNN.py` imports a name R (5 import forms), defines its own
`go` (and a class `Local` whose `go`/`sub` lead to it), adds 0-2 extra
occurrences of R (bindings, attribute writes, reads, dynamic patches) at module
level, inside `main` or inside a helper function, then calls `R.go()` or
`R.sub.go()` once in `main`. `ext.py` / `ext2.py` / `pkg/ext.py` print EXT /
EXT2 / PKG; the file's own `go` prints LOCAL. Running a case with CPython
prints which `go` that one call ran.
"""
import json, os, random, sys

OUT, N, SEED = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rnd = random.Random(SEED)
LIB = {
    "ext.py": 'def go():\n    print("EXT")\n\nclass sub:\n    go = staticmethod(go)\n\nclass C:\n    go = staticmethod(go)\n    sub = sub\n',
    "ext2.py": 'def go():\n    print("EXT2")\n\nclass sub:\n    go = staticmethod(go)\n',
    "pkg/__init__.py": "",
    "pkg/ext.py": 'def go():\n    print("PKG")\n\nclass sub:\n    go = staticmethod(go)\n',
    "pkg2/__init__.py": "",
    "pkg2/other.py": 'def go():\n    print("OTHER")\n\nclass sub:\n    go = staticmethod(go)\n',
}
# In package pkg2, as module pkg2.<SELF>: (import line, R, call shapes).
SELF_IMPORTS = [
    # `pkg2.<self>` exists once something imported it, as in real code.
    ("import pkg2\nimport pkg2.SELF as _loaded", "pkg2", ["pkg2.SELF.go()", "pkg2.SELF.Local.go()", "pkg2.other.go()"]),
    ("import pkg2.SELF", "pkg2", ["pkg2.SELF.go()", "pkg2.SELF.Local.go()"]),
    ("import pkg2.SELF as R", "R", ["R.go()", "R.Local.go()"]),
    ("from pkg2 import SELF as R", "R", ["R.go()", "R.sub.go()"]),
    ("from pkg2 import other as R", "R", ["R.go()", "R.sub.go()"]),
    ("from pkg2 import SELF", "SELF", ["SELF.go()"]),
]
# (import line, R, call shapes)
IMPORTS = [
    ("import ext as R", "R", ["R.go()", "R.sub.go()"]),
    ("import ext", "ext", ["ext.go()", "ext.sub.go()"]),
    ("from ext import C as R", "R", ["R.go()", "R.sub.go()"]),
    ("from ext import sub", "sub", ["sub.go()"]),
    ("import pkg.ext", "pkg", ["pkg.ext.go()", "pkg.ext.sub.go()"]),
]
# Extra occurrences of R; kind: bind / write / read / dyn (documented misses).
OCC = [
    ("bind", "R = Local"), ("bind", "R: type = Local"), ("bind", "R: int"),
    ("bind", "R, _x = Local, 1"), ("bind", "[R] = [Local]"), ("bind", "*R, = [Local]"),
    ("bind", "R += 0"), ("bind", "for R in [Local]:\n    pass"),
    ("bind", "with Ctx() as R:\n    pass"), ("bind", "if (R := Local):\n    pass"),
    ("bind", "def R():\n    pass\nR.go = go\nR.sub = Local"),
    ("bind", "class R:\n    go = staticmethod(go)\n    sub = Local"),
    ("bind", "del R"), ("bind", "match Local:\n    case R:\n        pass"),
    ("bind", "import ext2 as R"), ("bind", "from ext import C as R"),
    ("bind", "_f = lambda R: R"), ("bind", "_c = [R for R in [Local]]"),
    ("bind", "def _o(R=Local):\n    return R"), ("bind", "type R = int"),
    ("bind", "global R\nR = Local"),
    ("write", "R.go = go"), ("write", "R.sub.go = go"), ("write", "R.sub = Local"),
    ("write", "del R.go"), ("write", "R.n = 1"), ("write", "(R).go = go"),
    ("write", "R.go, _y = go, 1"), ("write", "for R.go in [go]:\n    pass"),
    ("write", "with Ctx2() as R.go:\n    pass"), ("write", "R.sub.go += 0"),
    ("read", "print(R is None, file=_sink)"), ("read", "_g = getattr(R, 'go', None)"),
    ("read", "_h = hasattr(R, 'x')"), ("read", "_l = [R, (R,)]"), ("read", "_d = {R: R}"),
    ("read", "_s = f'{R}'"), ("read", "assert R"), ("read", "_t = R.sub"),
    ("read", "def _p(x=R.sub):\n    return x"), ("read", "_i = isinstance(1, (R.sub,))"),
    ("read", "if R and not R:\n    pass"),
    ("dyn", "setattr(R, 'go', go)"), ("dyn", "vars(R)['go'] = go"),
    ("dyn", "globals()['R'] = Local"),
]
PRE = ('import io\n_sink = io.StringIO()\n\ndef go():\n    print("LOCAL")\n\n'
       'class Local:\n    go = staticmethod(go)\n\nLocal.sub = Local\n\n'
       'class Ctx:\n    def __enter__(self):\n        return Local\n    def __exit__(self, *a):\n        return False\n\n'
       'class Ctx2:\n    def __enter__(self):\n        return go\n    def __exit__(self, *a):\n        return False\n\n')

def indent(s, n):
    return "\n".join(" " * n + l for l in s.split("\n"))

def case(i):
    in_pkg = rnd.random() < 0.35
    imp, R, calls = rnd.choice(SELF_IMPORTS if in_pkg else IMPORTS)
    call = rnd.choice(calls)
    me = f"c{i:05d}"
    imp, R, call = imp.replace("SELF", me), R.replace("SELF", me), call.replace("SELF", me)
    occ = [(k, s.replace("R", R) if R != "R" else s, rnd.choice(["mod", "main", "fn"]))
           for k, s in rnd.sample(OCC, rnd.choice([0, 1, 1, 2]))]
    # `R` inside templates is the placeholder; protect identifiers containing R.
    src = imp + "\n" + PRE
    main_pre = []
    for k, s, where in occ:
        if where == "mod":
            src += "try:\n" + indent(s, 4) + "\nexcept Exception:\n    pass\n\n"
        elif where == "fn":
            src += "def _other():\n" + indent(s, 4) + "\n\ntry:\n    _other()\nexcept Exception:\n    pass\n\n"
        else:
            main_pre.append(s)
    body = "".join("    try:\n" + indent(s, 8) + "\n    except Exception:\n        pass\n" for s in main_pre)
    src += "def main():\n" + body + "    try:\n        " + call + "\n    except Exception as e:\n        print('ERR', type(e).__name__)\n\nif __name__ == '__main__':\n    main()\n"
    return src, {"import": imp, "call": call, "occ": [[k, s, w] for k, s, w in occ], "pkg": in_pkg}

os.makedirs(OUT, exist_ok=True)
for name, body in LIB.items():
    os.makedirs(os.path.dirname(os.path.join(OUT, name)) or OUT, exist_ok=True)
    open(os.path.join(OUT, name), "w").write(body)
meta = {}
for i in range(N):
    src, m = case(i)
    try:
        compile(src, "c", "exec")
    except SyntaxError:
        continue
    name = ("pkg2/" if m["pkg"] else "") + f"c{i:05d}.py"
    open(os.path.join(OUT, name), "w").write(src)
    meta[name] = m
json.dump(meta, open(os.path.join(OUT, "cases.json"), "w"), indent=0)
print(len(meta), "cases")
