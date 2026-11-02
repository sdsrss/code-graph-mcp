BEFORE = {
 "pkg/__init__.py": "from .m import Y as X\n",
 "pkg/m.py": "def Y():\n    pass\n",
 "app/c.py": "import pkg\nfrom pkg import X, Y\n\ndef run():\n    pkg.X()\n    X()\n    pkg.Y()\n    Y()\n",
 "app/other.py": "def X():\n    pass\n\ndef Y():\n    pass\n\ndef Z():\n    pass\n",
}
STEPS = [{"pkg/m.py": "def Z():\n    pass\n"}, {"pkg/m.py": "def Y():\n    pass\n"}]
SHOW = ["app/c.py"]
