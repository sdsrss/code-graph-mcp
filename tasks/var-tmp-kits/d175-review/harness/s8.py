SCENARIOS = {}
SCENARIOS["bound_method_alias_with_nested_namesake"] = [
    {"m.py": "class Registry:\n    def register(self, f):\n        return f\n\n_default = Registry()\nregister = _default.register\n\n\ndef make():\n    def register(x):\n        return x\n    return register\n",
     "user.py": "from m import register\n\n@register\ndef f():\n    pass\n\ndef g():\n    register(f)\n"},
]
SCENARIOS["d517_calls_through_import"] = [
    {"pkg/__init__.py": "", "pkg/m.py": "class C:\n    def Y(self):\n        pass\n\nclass D:\n    def Y(self):\n        pass\n",
     "app2.py": "from pkg.m import Y\n\ndef main2():\n    Y()\n", "zzz/decoy.py": "def Y():\n    pass\n"},
    {"pkg/m.py": "def Y():\n    return 1\n\nclass C:\n    def Y(self):\n        pass\n"},
    {"pkg/m.py": "# e\ndef Y():\n    return 1\n\nclass C:\n    def Y(self):\n        pass\n"},
    {"pkg/m.py": "class C:\n    def Y(self):\n        pass\n\nclass D:\n    def Y(self):\n        pass\n"},
]
