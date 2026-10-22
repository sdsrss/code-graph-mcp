SCENARIOS = {}
SCENARIOS["chain_delete_inner"] = [
    {"a/__init__.py": "from a.b import X\n", "a/b/__init__.py": "from a.b.c import X\n",
     "a/b/c.py": "def X():\n    pass\n", "z.py": "def X():\n    pass\n",
     "user.py": "from a import X\n\ndef go():\n    X()\n"},
    {"a/b/c.py": None},
    {"a/b/c.py": "def X():\n    pass\n"},
]
SCENARIOS["submodule_appears_beside_reexport"] = [
    {"pkg/__init__.py": "from .impl import X\n", "pkg/impl.py": "def X():\n    pass\n",
     "pkg/a.py": "from . import X\n\ndef f():\n    X()\n"},
    {"pkg/X.py": "def y():\n    pass\n"},
    {"pkg/X.py": None},
]
SCENARIOS["rename_reexporter_dir"] = [
    {"pkg/__init__.py": "from .impl import X\n", "pkg/impl.py": "def X():\n    pass\n",
     "user.py": "from pkg import X\nfrom pkg2 import X as X2\n\ndef go():\n    X()\n    X2()\n"},
    {"pkg/__init__.py": None, "pkg/impl.py": None, "pkg2/__init__.py": "from .impl import X\n", "pkg2/impl.py": "def X():\n    pass\n"},
    {"pkg2/__init__.py": None, "pkg2/impl.py": None, "pkg/__init__.py": "from .impl import X\n", "pkg/impl.py": "def X():\n    pass\n"},
]
SCENARIOS["reexported_fn_gains_method_namesake_elsewhere"] = [
    {"pkg/__init__.py": "from .impl import load\n", "pkg/impl.py": "def load():\n    pass\n",
     "user.py": "from pkg import load\n\ndef go():\n    load()\n", "other.py": "class R:\n    def load(self):\n        pass\n",
     "caller.py": "def h(r):\n    return r.inner.load()\n"},
    {"pkg/impl.py": "def load():\n    pass\n\nclass L:\n    def load(self):\n        pass\n"},
    {"pkg/impl.py": "# c\ndef load():\n    pass\n\nclass L:\n    def load(self):\n        pass\n"},
]
SCENARIOS["namespace_relative_reexport"] = [
    {"ns/sub/__init__.py": "from .impl import X\n", "ns/sub/impl.py": "def X():\n    pass\n",
     "ns/user.py": "from .sub import X\n\ndef go():\n    X()\n", "ns/user2.py": "from ns.sub import X\n"},
    {"ns/sub/__init__.py": "from .impl import X as X\n"},
    {"ns/sub/__init__.py": ""},
    {"ns/sub/__init__.py": "from .impl import X\n"},
]
SCENARIOS["nested_closure_namesake"] = [
    {"lib.py": "def factory():\n    def handle():\n        pass\n    return handle\n\nclass H:\n    def handle(self):\n        pass\n",
     "app.py": "def run(x):\n    x.h.handle()\n\ndef run2(x):\n    return handle()\n"},
    {"lib.py": "# edit\ndef factory():\n    def handle():\n        pass\n    return handle\n\nclass H:\n    def handle(self):\n        pass\n"},
]
