import itertools

FN = "def Y():\n    return 1\n\n"
NEST = "def make():\n    def Y():\n        return 1\n    return Y\n\n"
CLS = "class Y:\n    pass\n\n"
CY = "class C:\n    def Y(self):\n        return 2\n\n"
DY = "class D:\n    def Y(self):\n        return 3\n\n"
CNEST = "class C:\n    class Y:\n        pass\n\n"
OUTER = "class Outer:\n    class C:\n        def Y(self):\n            return 4\n\n"
STATIC = "class C:\n    @staticmethod\n    def Y():\n        return 2\n\n"
TWOFN = "import sys\nif sys:\n    def Y():\n        return 1\nelse:\n    def Y():\n        return 0\n\n"
OTHER = "def other():\n    return 0\n\n"

V = {
    "none": OTHER,
    "fn": FN,
    "cy": CY,
    "fn_cy": FN + CY,
    "nest_cy": NEST + CY,
    "cls_cy": CLS + CY,
    "cy_dy": CY + DY,
    "fn_dy": FN + DY,
    "fn_cy_dy": FN + CY + DY,
    "nest": NEST,
    "cls": CLS,
    "cnest_dy": CNEST + DY,
    "fn_outer": FN + OUTER,
    "fn_static": FN + STATIC,
    "twofn_cy": TWOFN + CY,
    "cy_fn": CY + FN,
}

CALLER = (
    "import pkg.m\n"
    "import pkg.m as mm\n"
    "from pkg import m\n"
    "\n"
    "def member(x):\n    return x.store.Y()\n\n"
    "def via_mod():\n    return m.Y()\n\n"
    "def via_alias():\n    return mm.Y()\n\n"
    "def via_dotted():\n    return pkg.m.Y()\n\n"
    "def bare():\n    return Y()\n\n"
    "def val_ref():\n    cb = Y\n    return cb\n\n"
    "def arg_ref(f):\n    return f(Y)\n\n"
    "def ann(x: Y) -> Y:\n    return x\n\n"
    "@Y\ndef decorated():\n    pass\n"
)
CALLER_IMP = "from pkg.m import Y\n\ndef imp_call():\n    return Y()\n\ndef imp_ref():\n    return Y\n"
CALLER_CLS = (
    "from pkg.m import C\n\n"
    "def cls_call():\n    return C.Y(None)\n\n"
    "def inst_call():\n    return C().Y()\n\n"
    "def typed(c: C):\n    return c.Y()\n\n"
    "class E(C):\n    def g(self):\n        return self.Y()\n\n    def h(self):\n        return super().Y()\n"
)
CALLER_STAR = "from pkg.m import *\n\ndef star_call():\n    return Y()\n"

def tree(m):
    return {"pkg/__init__.py": "", "pkg/m.py": m, "app.py": CALLER, "app_imp.py": CALLER_IMP,
            "app_cls.py": CALLER_CLS, "app_star.py": CALLER_STAR}

CASES = {}
for a, b in itertools.permutations(V, 2):
    CASES[f"{a}__{b}"] = {"init": tree(V[a]), "steps": [{"pkg/m.py": V[b]}]}
for a in V:
    CASES[f"{a}__comment"] = {"init": tree(V[a]), "steps": [{"pkg/m.py": "# edit\n" + V[a]}]}
