BEFORE = {
 "pkg/__init__.py": "from .m import Y as X\n",
 "pkg/m.py": "def X():\n    pass\n\ndef Y():\n    pass\n",
 "app/c.py": "import pkg\nfrom pkg import X\n\ndef run():\n    pkg.X()\n    X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "from .m import X\n"}, {"pkg/__init__.py": "from .m import Y as X\n"}]
SHOW = ["app/c.py"]
