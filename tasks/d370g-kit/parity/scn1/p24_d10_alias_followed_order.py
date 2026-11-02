BEFORE = {
 "pkg/__init__.py": "from .m import Y as X\n",
 "pkg/m.py": "Y = 1\n\ndef other():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
 "app2.py": "from pkg import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"app2.py": "from pkg import X\n\n\ndef main2():\n    X()\n"}]
SHOW = ["app2"]
