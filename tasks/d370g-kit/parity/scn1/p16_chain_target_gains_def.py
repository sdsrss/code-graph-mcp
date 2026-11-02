BEFORE = {
 "pkg/__init__.py": "from .sub import X\n",
 "pkg/sub/__init__.py": "from .impl import X\n",
 "pkg/sub/impl.py": "def Y():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
 "app2.py": "from pkg import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"pkg/sub/impl.py": "def Y():\n    pass\n\ndef X():\n    pass\n"}]
SHOW = ["app"]
