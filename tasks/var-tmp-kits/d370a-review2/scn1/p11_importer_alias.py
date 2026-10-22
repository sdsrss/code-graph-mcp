BEFORE = {
 "pkg/__init__.py": "from .core import X\n",
 "pkg/core.py": "def X():\n    pass\n",
 "pkg/core2.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "app.py": "import pkg as p\n\ndef main():\n    p.X()\n",
 "app2.py": "from pkg import X as Y\n\ndef main2():\n    Y()\n",
}
STEPS = [{"pkg/__init__.py": "from .core2 import X\n"}]
