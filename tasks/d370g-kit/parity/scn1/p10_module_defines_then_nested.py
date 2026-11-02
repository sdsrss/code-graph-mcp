BEFORE = {
 "pkg/__init__.py": "from .core import X\n",
 "pkg/core.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
}
STEPS = [{"pkg/__init__.py": "from .core import X\n\ndef f():\n    def X():\n        pass\n    return X\n"}]
