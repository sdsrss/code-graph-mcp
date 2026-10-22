# relative from .m import X where m re-exports from .impl; impl renames X, other file gains X, m retargets
BEFORE = {
 "pkg/__init__.py": "",
 "pkg/m.py": "from .impl import X\n",
 "pkg/impl.py": "def X():\n    pass\n",
 "pkg/impl2.py": "def Y():\n    pass\n",
 "pkg/sub.py": "from .m import X\nfrom . import m\n\ndef run():\n    X()\n    m.X()\n",
 "other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/impl.py": "def Z():\n    pass\n"}, {"pkg/impl2.py": "def X():\n    pass\n", "pkg/m.py": "from .impl2 import X\n"}]
SHOW = ["pkg/sub.py"]
