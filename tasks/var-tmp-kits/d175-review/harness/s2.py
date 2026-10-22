SCENARIOS = {}
def chain(n, inner):
    # p0/__init__ re-exports from p1, ..., p{n-1}/__init__ re-exports from p{n-1}/<inner>
    t = {}
    for i in range(n - 1):
        t[f"p{i}/__init__.py"] = f"from p{i+1} import X\n"
    t[f"p{n-1}/__init__.py"] = f"from .{inner} import X\n"
    t[f"p{n-1}/c.py"] = "def X():\n    pass\n"
    t[f"p{n-1}/d.py"] = "def X():\n    pass\n"
    t["user.py"] = "from p0 import X\n\ndef go():\n    X()\n"
    return t
for n in (8, 9, 10, 11):
    SCENARIOS[f"chain{n}"] = [chain(n, "c"), {f"p{n-1}/__init__.py": "from .d import X\n"}]
SCENARIOS["cycle"] = [
    {"a/__init__.py": "from b import X\n", "b/__init__.py": "from a import X\n", "x.py": "def X():\n    pass\n",
     "user.py": "from a import X\n\ndef go():\n    X()\n"},
    {"b/__init__.py": "from a import X\nfrom x import X\n"},
    {"b/__init__.py": "from x import X\n"},
    {"b/__init__.py": "from a import X\n"},
]
SCENARIOS["core_redefines"] = [
    {"pkg/__init__.py": "from .core import X\n", "pkg/core.py": "from .impl import X\n",
     "pkg/impl.py": "def X():\n    pass\n", "user.py": "from pkg import X\n\ndef go():\n    X()\n"},
    {"pkg/core.py": "def X():\n    pass\n"},
    {"pkg/core.py": "from .impl import X\n"},
]
SCENARIOS["rename_target_file"] = [
    {"pkg/__init__.py": "from .core import X\n", "pkg/core.py": "def X():\n    pass\n",
     "user.py": "from pkg import X\n\ndef go():\n    X()\n", "x.py": "def X():\n    pass\n"},
    {"pkg/core.py": None, "pkg/core2.py": "def X():\n    pass\n", "pkg/__init__.py": "from .core2 import X\n"},
    {"pkg/core2.py": None},
]
SCENARIOS["add_reexport_rel_importer"] = [
    {"pkg/__init__.py": "", "pkg/app.py": "class Flask:\n    pass\n",
     "pkg/cli.py": "from . import Flask\n\ndef main():\n    Flask()\n",
     "pkg/sub/__init__.py": "", "pkg/sub/m.py": "from .. import Flask\n\ndef f():\n    return Flask()\n",
     "tests/conftest.py": "from pkg import Flask\n\ndef t():\n    class Flask(Flask):\n        pass\n    return Flask()\n",
     "tests/test_a.py": "from pkg import Flask\n\ndef test_a():\n    Flask()\n"},
    {"pkg/__init__.py": "from .app import Flask as Flask\n"},
    {"pkg/__init__.py": "from .app import Flask as F\n"},
    {"pkg/__init__.py": ""},
]
SCENARIOS["root_module_appears"] = [
    {"tests/helpers.py": "def h():\n    pass\n", "tests/test_a.py": "import helpers\nfrom helpers import h\n\ndef test_x():\n    h()\n"},
    {"helpers.py": "def h():\n    pass\n"},
    {"helpers.py": None},
]
SCENARIOS["nested_init_appears"] = [
    {"pkg/__init__.py": "", "pkg/sub/x.py": "def h():\n    pass\n",
     "pkg/a.py": "from .sub import x\nfrom pkg.sub import x as y\nfrom .sub.x import h\nimport pkg.sub\n\ndef f():\n    h()\n"},
    {"pkg/sub/__init__.py": "from .x import h\n"},
    {"pkg/sub/__init__.py": None},
]
SCENARIOS["tests_init_reexport"] = [
    {"tests/helpers.py": "def h():\n    pass\n", "tests/test_a.py": "from helpers import h\nimport helpers\n\ndef test_x():\n    h()\n",
     "src/pkg/__init__.py": "from .core import h\n", "src/pkg/core.py": "def h():\n    pass\n",
     "tests/test_b.py": "from pkg import h\n\ndef test_y():\n    h()\n"},
    {"tests/__init__.py": "from .helpers import h\n"},
    {"src/__init__.py": ""},
    {"tests/__init__.py": None, "src/__init__.py": None},
]
