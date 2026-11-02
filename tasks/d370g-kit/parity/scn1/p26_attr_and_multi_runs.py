# several runs: add re-export, then rename target, then restore
BEFORE = {
 "pkg/__init__.py": "def other():\n    pass\n",
 "pkg/m.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
 "app2.py": "from pkg import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"pkg/__init__.py": "from .m import X\n"}, {"pkg/m.py": "def Y():\n    pass\n"}, {"pkg/m.py": "def X():\n    pass\n"}]
SHOW = ["app"]
