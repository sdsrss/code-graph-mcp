BEFORE = {
 "a/__init__.py": "from .b import X\n",
 "a/b/__init__.py": "from .c import X\n",
 "a/b/c/__init__.py": "from .d import X\n",
 "a/b/c/d.py": "def Y():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
 "app.py": "import a\n\ndef main():\n    a.X()\n",
 "app2.py": "from a import X\n\ndef main2():\n    X()\n",
}
STEPS = [{"a/b/c/d.py": "def Y():\n    pass\n\ndef X():\n    pass\n"}]
SHOW = ["app"]
