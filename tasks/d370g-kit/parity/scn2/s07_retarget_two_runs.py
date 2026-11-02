BEFORE = {
 "pkg/__init__.py": "from .m import X\n",
 "pkg/m.py": "def X():\n    pass\n",
 "app/c.py": "import pkg\nfrom pkg import X\n\ndef run():\n    pkg.X()\n    X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/m.py": None, "pkg/n.py": "def X():\n    pass\n"}, {"pkg/__init__.py": "from .n import X\n"}]
SHOW = ["app/c.py"]
