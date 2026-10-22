# D#518 incremental scenarios: SCENARIOS = {name: [step0, step1, ...]}
CALLER = "import m\n\ndef f():\n    return m.X()\n"
S = {}
S["a01_module_gains_X"] = [
    {"m.py": "def other():\n    pass\n", "q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"m.py": "def other():\n    pass\n\ndef X():\n    pass\n"},
    {"m.py": "def other():\n    pass\n"},
]
S["a02_module_loses_X"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"m.py": "def other():\n    pass\n"},
    {"m.py": "def X():\n    return 1\n"},
]
S["a03_function_to_method"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"m.py": "class C:\n    def X(self):\n        pass\n"},
    {"m.py": "class C:\n    def X(self):\n        pass\n\ndef X():\n    pass\n"},
    {"m.py": "def X():\n    pass\n"},
]
S["a04_decoy_added_removed"] = [
    {"m.py": "def X():\n    pass\n", "app.py": CALLER},
    {"q.py": "def X():\n    pass\n"},
    {"r.py": "class K:\n    def X(self):\n        pass\n"},
    {"q.py": None},
    {"r.py": None},
]
S["a05_reexport_retarget"] = [
    {"pkg/__init__.py": "from .core import X\n", "pkg/core.py": "def X():\n    pass\n", "pkg/other.py": "def X():\n    pass\n",
     "app.py": "import pkg\n\ndef f():\n    return pkg.X()\n"},
    {"pkg/__init__.py": "from .other import X\n"},
    {"pkg/__init__.py": "VERSION = 1\n"},
    {"pkg/__init__.py": "from .core import X\n"},
    {"pkg/core.py": "def Y():\n    pass\n"},
    {"pkg/core.py": "def X():\n    pass\n"},
]
S["a06_init_deleted_added"] = [
    {"pkg/__init__.py": "def X():\n    pass\n", "pkg/core.py": "def X():\n    pass\n", "lib.py": "def X():\n    pass\n",
     "app.py": "import pkg\n\ndef f():\n    return pkg.X()\n"},
    {"pkg/__init__.py": None},
    {"pkg/__init__.py": "def X():\n    pass\n"},
    {"pkg/__init__.py": "from .core import X\n"},
    {"pkg/__init__.py": None},
    {"pkg/__init__.py": "from .core import X\n"},
]
S["a07_module_renamed_caller_unchanged"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"m.py": None, "m/__init__.py": "def X():\n    pass\n"},
    {"m/__init__.py": None, "m2.py": "def X():\n    pass\n"},
    {"m2.py": None, "m.py": "def X():\n    pass\n"},
]
S["a08_caller_rebinds_name"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"app.py": CALLER + "\nm = None\n"},
    {"app.py": CALLER},
    {"app.py": "import m\n\ndef f(m):\n    return m.X()\n"},
    {"app.py": CALLER},
]
S["a09_same_fn_PX_and_bare_X_from"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n",
     "app.py": "import m\nfrom q import X\n\ndef f():\n    m.X()\n    X()\n"},
    {"m.py": "def Y():\n    pass\n"},
    {"m.py": "def X():\n    pass\n"},
    {"q.py": "def Z():\n    pass\n"},
    {"q.py": "def X():\n    pass\n"},
    {"m.py": None},
    {"m.py": "def X():\n    pass\n"},
]
S["a10_same_fn_PX_and_bare_local"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n",
     "app.py": "import m\n\ndef X():\n    pass\n\ndef f():\n    m.X()\n    X()\n"},
    {"m.py": "def Y():\n    pass\n"},
    {"m.py": "def X():\n    pass\n"},
    {"app.py": "import m\n\ndef f():\n    m.X()\n    X()\n"},
    {"m.py": None},
    {"m.py": "def X():\n    pass\n"},
]
S["a11_same_fn_PX_bare_X_unresolved"] = [
    {"m.py": "def other():\n    pass\n",
     "app.py": "import m\n\ndef f():\n    m.X()\n    X()\n"},
    {"m.py": "def X():\n    pass\n"},
    {"q.py": "def X():\n    pass\n"},
    {"m.py": "def other():\n    pass\n"},
    {"q.py": None},
]
S["a12_two_modules_same_attr"] = [
    {"m.py": "def X():\n    pass\n", "n.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n",
     "app.py": "import m, n\n\ndef f():\n    m.X()\n    n.X()\n"},
    {"m.py": "def Y():\n    pass\n"},
    {"n.py": "def Y():\n    pass\n"},
    {"m.py": "def X():\n    pass\n"},
    {"n.py": "def X():\n    pass\n"},
]
S["a13_nested_and_class_X"] = [
    {"m.py": "def X():\n    pass\n\ndef outer():\n    def X():\n        pass\n    return X\n", "q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"m.py": "def outer():\n    def X():\n        pass\n    return X\n"},
    {"m.py": "class X:\n    pass\n"},
    {"m.py": "class Outer:\n    class X:\n        pass\n"},
    {"m.py": "def X():\n    pass\n"},
]
S["a14_import_as_dotted"] = [
    {"pkg/__init__.py": "", "pkg/core.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n",
     "app.py": "import pkg.core as c\n\ndef f():\n    return c.X()\n"},
    {"pkg/core.py": "def Y():\n    pass\n"},
    {"pkg/core.py": "def X():\n    pass\n"},
    {"pkg/__init__.py": None},
    {"pkg/__init__.py": ""},
]
S["a15_external_becomes_internal"] = [
    {"q.py": "def X():\n    pass\n", "app.py": "import ext\n\ndef f():\n    return ext.X()\n"},
    {"ext.py": "def X():\n    pass\n"},
    {"ext.py": None},
    {"ext.py": "def Y():\n    pass\n"},
    {"ext.py": "def X():\n    pass\n"},
]
S["a16_name_same_as_module"] = [
    {"X.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n", "app.py": "import X\n\ndef f():\n    return X.X()\n"},
    {"X.py": "def Y():\n    pass\n"},
    {"X.py": "def X():\n    pass\n"},
]
S["a17_P_X_and_from_P_import_X"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n",
     "app.py": "import m\nfrom m import X\n\ndef f():\n    m.X()\n    X()\n"},
    {"m.py": "def Y():\n    pass\n"},
    {"m.py": "def X():\n    pass\n"},
]
S["a18_reexport_chain"] = [
    {"pkg/__init__.py": "from .sub import X\n", "pkg/sub/__init__.py": "from .impl import X\n", "pkg/sub/impl.py": "def X():\n    pass\n",
     "q.py": "def X():\n    pass\n", "app.py": "import pkg\n\ndef f():\n    return pkg.X()\n"},
    {"pkg/sub/impl.py": "def Y():\n    pass\n"},
    {"pkg/sub/impl.py": "def X():\n    pass\n"},
    {"pkg/sub/__init__.py": "from .impl import Y as X\n"},
    {"pkg/sub/__init__.py": "from .impl import X\n"},
    {"pkg/sub/impl.py": "class K:\n    def X(self):\n        pass\n"},
    {"pkg/sub/impl.py": "def X():\n    pass\n"},
]
S["a19_module_added_to_index"] = [
    {"q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"m.py": "def X():\n    pass\n"},
    {"m.py": "def Z():\n    pass\n"},
    {"m.py": None},
]
S["a20_caller_added_later"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n"},
    {"app.py": CALLER},
    {"app2.py": "import m\n\ndef g():\n    return m.X()\n\ndef X():\n    pass\n"},
    {"app.py": None},
]
S["a21_method_on_reexport"] = [
    {"pkg/__init__.py": "from .core import X\n", "pkg/core.py": "class C:\n    def X(self):\n        pass\n", "q.py": "def X():\n    pass\n",
     "app.py": "import pkg\n\ndef f():\n    return pkg.X()\n"},
    {"pkg/core.py": "def X():\n    pass\n"},
    {"pkg/core.py": "class C:\n    def X(self):\n        pass\n"},
]
S["a22_star_reexport"] = [
    {"pkg/__init__.py": "from .core import *\n", "pkg/core.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n",
     "app.py": "import pkg\n\ndef f():\n    return pkg.X()\n"},
    {"pkg/core.py": "def Y():\n    pass\n"},
    {"pkg/core.py": "def X():\n    pass\n"},
]
S["a23_rename_caller_file"] = [
    {"m.py": "def X():\n    pass\n", "q.py": "def X():\n    pass\n", "app.py": CALLER},
    {"app.py": None, "app_renamed.py": CALLER},
    {"m.py": None, "mm.py": "def X():\n    pass\n"},
]
S["a24_attr_name_is_class_ctor"] = [
    {"m.py": "class X:\n    def __init__(self):\n        pass\n", "q.py": "class X:\n    pass\n", "app.py": CALLER},
    {"m.py": "class X:\n    def __init__(self, a=1):\n        pass\n\nclass Y:\n    pass\n"},
    {"m.py": "Y = 1\n"},
    {"m.py": "class X:\n    pass\n"},
]
SCENARIOS = S
