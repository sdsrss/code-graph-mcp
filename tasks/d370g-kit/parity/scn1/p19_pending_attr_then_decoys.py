BEFORE = {
 "pkg/__init__.py": "def other():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
}
STEPS = [{"zzz/decoy.py": "def X():\n    pass\n", "yyy/decoy2.py": "def X():\n    pass\n"}]
SHOW = ["app.py:main"]
