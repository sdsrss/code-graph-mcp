# absolute chain: m.py gains a def X shadowing its own re-export of impl.X
BEFORE = {
 "pkg/__init__.py": "from pkg.m import X\n",
 "pkg/m.py": "from pkg.impl import X\n",
 "pkg/impl.py": "def X():\n    pass\n",
 "user.py": "from pkg import X\nimport pkg\n\ndef a():\n    X()\n\ndef b():\n    pkg.X()\n",
}
STEPS = [{"pkg/m.py": "from pkg.impl import X\n\ndef X():\n    pass\n"}]
SHOW = ["user.py", "pkg/__init__"]
