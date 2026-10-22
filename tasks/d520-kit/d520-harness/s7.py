SCENARIOS = {}
base = {"a/__init__.py": "from a.b import X\n", "a/b/__init__.py": "from a.b.c import X\n",
        "a/b/c.py": "def X():\n    pass\n", "a/b/d.py": "def X():\n    pass\n",
        "user.py": "from a import X\n\ndef go():\n    X()\n"}
SCENARIOS["chain_inner_and_user_same_run"] = [dict(base),
    {"a/b/__init__.py": "from a.b.d import X\n", "user.py": "from a import X\n\ndef go():\n    X()\n    X()\n"}]
SCENARIOS["chain_outer_and_inner_same_run"] = [dict(base),
    {"a/b/__init__.py": "from a.b.d import X\n", "a/__init__.py": "# c\nfrom a.b import X\n"}]
SCENARIOS["chain_target_and_user"] = [dict(base),
    {"a/b/c.py": "def Y():\n    pass\n", "user.py": "from a import X\n\ndef go():\n    X()\n    X()\n"}]
SCENARIOS["chain_init_vanish_mid"] = [dict(base),
    {"a/b/__init__.py": None}, {"a/b/__init__.py": "from a.b.c import X\n"}]
