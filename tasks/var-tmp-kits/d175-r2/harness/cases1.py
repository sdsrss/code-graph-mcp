M_BOTH = "def Y():\n    return 1\n\nclass C:\n    def Y(self):\n        return 2\n"
M_METH = "class C:\n    def Y(self):\n        return 2\n"
M_FN = "def Y():\n    return 1\n"
M_NESTED = "def make():\n    def Y():\n        return 1\n    return Y\n\nclass C:\n    def Y(self):\n        return 2\n"
M_CLASS = "class Y:\n    pass\n\nclass C:\n    def Y(self):\n        return 2\n"
M_BOTH_D = "def Y():\n    return 1\n\nclass D:\n    def Y(self):\n        return 2\n"

# callers: one per shape, none imports anything from m that changes
CALLER = (
    "import pkg.m\n"
    "from pkg import m\n"
    "\n"
    "def member(x):\n    return x.store.Y()\n\n"
    "def via_mod():\n    return m.Y()\n\n"
    "def via_dotted():\n    return pkg.m.Y()\n\n"
    "def bare():\n    return Y()\n\n"
    "def cls_call():\n    return pkg.m.C.Y(None)\n\n"
    "def val_ref():\n    cb = Y\n    return cb\n\n"
    "def arg_ref(f):\n    return f(Y)\n"
)
CALLER_IMP = "from pkg.m import Y\n\ndef imp_call():\n    return Y()\n\ndef imp_ref():\n    return Y\n"

def tree(m, extra=None):
    t = {"pkg/__init__.py": "", "pkg/m.py": m, "app.py": CALLER, "app_imp.py": CALLER_IMP}
    if extra:
        t.update(extra)
    return t

def edit(t):
    return "# edit\n" + t

CASES = {
    "comment_both": {"init": tree(M_BOTH), "steps": [{"pkg/m.py": edit(M_BOTH)}]},
    "comment_nested": {"init": tree(M_NESTED), "steps": [{"pkg/m.py": edit(M_NESTED)}]},
    "comment_class": {"init": tree(M_CLASS), "steps": [{"pkg/m.py": edit(M_CLASS)}]},
    "meth_to_both": {"init": tree(M_METH), "steps": [{"pkg/m.py": M_BOTH}]},
    "both_to_meth": {"init": tree(M_BOTH), "steps": [{"pkg/m.py": M_METH}]},
    "fn_to_both": {"init": tree(M_FN), "steps": [{"pkg/m.py": M_BOTH}]},
    "both_to_fn": {"init": tree(M_BOTH), "steps": [{"pkg/m.py": M_FN}]},
    "meth_to_nested": {"init": tree(M_METH), "steps": [{"pkg/m.py": M_NESTED}]},
    "nested_to_meth": {"init": tree(M_NESTED), "steps": [{"pkg/m.py": M_METH}]},
    "both_to_nested": {"init": tree(M_BOTH), "steps": [{"pkg/m.py": M_NESTED}]},
    "meth_to_class": {"init": tree(M_METH), "steps": [{"pkg/m.py": M_CLASS}]},
    "class_rename": {"init": tree(M_BOTH), "steps": [{"pkg/m.py": M_BOTH_D}]},
    "cycle_fn_meth": {"init": tree(M_BOTH), "steps": [
        {"pkg/m.py": M_METH}, {"pkg/m.py": M_BOTH}, {"pkg/m.py": M_FN}, {"pkg/m.py": M_NESTED},
        {"pkg/m.py": M_CLASS}, {"pkg/m.py": M_BOTH_D}, {"pkg/m.py": M_BOTH}]},
}
