# re-exporter __init__ deleted: pkg becomes a namespace package
BEFORE = {
 "pkg/__init__.py": "from .m import X\n",
 "pkg/m.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
 "app2.py": "from pkg import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"pkg/__init__.py": None}]
