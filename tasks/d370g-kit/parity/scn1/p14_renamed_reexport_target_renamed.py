BEFORE = {
 "pkg/__init__.py": "from .m import Y as X\n",
 "pkg/m.py": "def Y():\n    pass\n",
 "zzz/decoy.py": "def Y():\n    pass\n\ndef X():\n    pass\n",
 "yyy/decoy2.py": "def Y():\n    pass\n\ndef X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
 "app2.py": "from pkg import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"pkg/m.py": "def Z():\n    pass\n"}]
SHOW = ["app"]
