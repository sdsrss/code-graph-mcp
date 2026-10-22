REG = ("class Registry:\n    def register(self, x):\n        pass\n\n_default = Registry()\nregister = _default.register\n\n"
       "def make():\n    def register(x):\n        pass\n    return register\n")
CASES = {
    "nc1_nested_registry": {"init": {"m.py": REG, "app.py": "from m import register\n\ndef main():\n    register(1)\n"}, "steps": []},
    "nc2_lazy_reexport": {"init": {"pkg/__init__.py": "def lazy():\n    from .heavy import X\n    return X\n",
                                    "pkg/heavy.py": "def X():\n    pass\n",
                                    "zzz/other.py": "def X():\n    pass\n",
                                    "app.py": "from pkg import X\n\ndef main():\n    X()\n"}, "steps": []},
    "nc3a_rel_parent_init_readd": {
        "init": {"pkg/__init__.py": "from .a import X\n", "pkg/a.py": "def X():\n    pass\n",
                 "pkg/sub/__init__.py": "", "pkg/sub/b.py": "from .. import X\n\ndef main():\n    X()\n",
                 "zzz/x.py": "def X():\n    pass\n"},
        "steps": [{"pkg/__init__.py": None}, {"pkg/__init__.py": "from .a import X\n"}]},
    "nc3a_rel_parent_init_readd_plain": {
        "init": {"pkg/__init__.py": "def X():\n    pass\n",
                 "pkg/sub/__init__.py": "", "pkg/sub/b.py": "from .. import X\n\ndef main():\n    X()\n"},
        "steps": [{"pkg/__init__.py": None}, {"pkg/__init__.py": "def X():\n    pass\n"}]},
    "nc3b_new_toplevel_helpers": {
        "init": {"tests/helpers.py": "def make():\n    pass\n", "tests/test_a.py": "import helpers\n\ndef test():\n    helpers.make()\n"},
        "steps": [{"helpers.py": "def make():\n    pass\n"}]},
    "nc3b_new_toplevel_helpers_from": {
        "init": {"tests/helpers.py": "def make():\n    pass\n", "tests/test_a.py": "from helpers import make\n\ndef test():\n    make()\n"},
        "steps": [{"helpers.py": "def make():\n    pass\n"}]},
}
