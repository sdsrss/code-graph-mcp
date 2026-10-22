SCENARIOS = {}
# A: D#517 restore filter applied to non-import python calls
SCENARIOS["restore_ur_call"] = [
    {"pkg/__init__.py": "",
     "pkg/m.py": "def fetch_rows():\n    return 1\n\nclass Store:\n    def fetch_rows(self):\n        return 2\n",
     "pkg/b.py": "from . import m\n\nclass User:\n    def run(self):\n        return self.store.fetch_rows()\n\n    def run2(self):\n        return m.fetch_rows()\n\n\ndef bare():\n    return fetch_rows()\n"},
    {"pkg/m.py": "# edit\ndef fetch_rows():\n    return 1\n\nclass Store:\n    def fetch_rows(self):\n        return 2\n"},
]
# A2: same with absolute import of the module and member call on a local
SCENARIOS["restore_ur_call_abs"] = [
    {"m.py": "def fetch_rows():\n    return 1\n\nclass Store:\n    def fetch_rows(self):\n        return 2\n",
     "b.py": "import m\n\nclass User:\n    def run(self):\n        return self.store.fetch_rows()\n\n\ndef f(s):\n    return s.fetch_rows()\n"},
    {"m.py": "# edit\ndef fetch_rows():\n    return 1\n\nclass Store:\n    def fetch_rows(self):\n        return 2\n"},
]
# C: delete a non-__init__ re-exporter
SCENARIOS["delete_reexporter"] = [
    {"pkg/__init__.py": "", "pkg/core.py": "def X():\n    pass\n", "pkg/api.py": "from .core import X\n",
     "user.py": "from pkg.api import X\n\ndef go():\n    X()\n"},
    {"pkg/api.py": None},
]
# D: retarget a non-__init__ re-exporter
SCENARIOS["retarget_reexporter"] = [
    {"pkg/__init__.py": "", "pkg/core.py": "def X():\n    pass\n", "pkg/other.py": "def X():\n    pass\n",
     "pkg/api.py": "from .core import X\n",
     "user.py": "from pkg.api import X\n\ndef go():\n    X()\n"},
    {"pkg/api.py": "from .other import X\n"},
]
# E: chain, inner re-exporter retargets
SCENARIOS["chain_retarget"] = [
    {"a/__init__.py": "from a.b import X\n", "a/b/__init__.py": "from a.b.c import X\n",
     "a/b/c.py": "def X():\n    pass\n", "a/b/d.py": "def X():\n    pass\n",
     "user.py": "from a import X\n\ndef go():\n    X()\n"},
    {"a/b/__init__.py": "from a.b.d import X\n"},
]
# E2: chain, inner target removed (X disappears from c.py)
SCENARIOS["chain_target_vanishes"] = [
    {"a/__init__.py": "from a.b import X\n", "a/b/__init__.py": "from a.b.c import X\n",
     "a/b/c.py": "def X():\n    pass\n", "x.py": "def X():\n    pass\n",
     "user.py": "from a import X\n\ndef go():\n    X()\n"},
    {"a/b/c.py": "def Y():\n    pass\n"},
    {"a/b/c.py": "def X():\n    pass\n"},
]
# G: relative imports in a dir that becomes a package
SCENARIOS["rel_dir_becomes_pkg"] = [
    {"tests/helpers.py": "def h():\n    pass\n",
     "tests/test_a.py": "from . import helpers\nfrom .helpers import h\nimport helpers as hh\n\ndef test_x():\n    h()\n    helpers.h()\n"},
    {"tests/__init__.py": ""},
    {"tests/__init__.py": None},
]
# H: src layout gains/loses src/__init__.py
SCENARIOS["src_init"] = [
    {"src/pkg/__init__.py": "from .mod import f\n", "src/pkg/mod.py": "def f():\n    pass\n",
     "tests/test_x.py": "import pkg\nfrom pkg.mod import f\nfrom pkg import mod\nfrom pkg import f as g\n\ndef test_a():\n    f()\n    g()\n"},
    {"src/__init__.py": ""},
    {"src/__init__.py": None},
]
# K: re-export of a name whose def becomes method-only
SCENARIOS["reexport_method_namesake"] = [
    {"pkg/__init__.py": "from .core import run\n",
     "pkg/core.py": "def run():\n    pass\n\nclass C:\n    def run(self):\n        pass\n",
     "user.py": "from pkg import run\n\ndef go():\n    run()\n"},
    {"pkg/core.py": "class C:\n    def run(self):\n        pass\n"},
    {"pkg/core.py": "def run():\n    pass\n\nclass C:\n    def run(self):\n        pass\n"},
]
# L: re-exporter gains a second source under try (n)
SCENARIOS["reexport_gains_try"] = [
    {"pkg/__init__.py": "from .core import X\n", "pkg/core.py": "def X():\n    pass\n",
     "pkg/fast.py": "def X():\n    pass\n",
     "user.py": "from pkg import X\n\ndef go():\n    X()\n"},
    {"pkg/__init__.py": "try:\n    from .fast import X\nexcept ImportError:\n    from .core import X\n"},
    {"pkg/__init__.py": "from .core import X\n"},
]
