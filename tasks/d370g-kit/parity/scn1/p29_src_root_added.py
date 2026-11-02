BEFORE = {
 "src/pkg/__init__.py": "from .core import X\n",
 "src/pkg/core.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
}
STEPS = [{"src/__init__.py": ""}]
SHOW = ["app.py"]
