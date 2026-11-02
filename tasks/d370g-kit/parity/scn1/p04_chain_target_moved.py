BEFORE = {
 "pkg/__init__.py": "from .sub import X\n",
 "pkg/sub/__init__.py": "from .impl import X\n",
 "pkg/sub/impl.py": "def X():\n    pass\n",
 "pkg/sub/impl2.py": "def Z():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
 "app2.py": "from pkg import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"pkg/sub/impl.py": "def Y():\n    pass\n", "pkg/sub/impl2.py": "def Z():\n    pass\n\ndef X():\n    pass\n", "pkg/sub/__init__.py": "from .impl2 import X\n"}]
SHOW = ["app"]
