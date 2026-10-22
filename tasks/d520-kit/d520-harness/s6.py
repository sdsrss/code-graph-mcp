SCENARIOS = {}
SCENARIOS["local_import_in_init_function"] = [
    {"pkg/__init__.py": "def lazy():\n    from .heavy import X\n    return X\n", "pkg/heavy.py": "def X():\n    pass\n",
     "other/__init__.py": "", "other/x.py": "def X():\n    pass\n",
     "user.py": "from pkg import X\n\ndef go():\n    X()\n"},
]
SCENARIOS["init_overrides_after_import"] = [
    {"pkg/__init__.py": "from .core import X\n\ndef X():\n    pass\n", "pkg/core.py": "def X():\n    pass\n",
     "user.py": "from pkg import X\n\ndef go():\n    X()\n"},
]
