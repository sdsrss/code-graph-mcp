BEFORE = {
 "pkg/__init__.py": "from .m import X\n\ndef X():\n    pass\n",
 "pkg/m.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
 "app2.py": "from pkg import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"pkg/__init__.py": "from .m import X\n"}]
SHOW = ["app"]
