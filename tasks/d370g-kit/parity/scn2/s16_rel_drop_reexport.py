BEFORE = {
 "pkg/__init__.py": "from .m import X\n",
 "pkg/m.py": "def X():\n    pass\n",
 "pkg/sub.py": "from . import X\n\ndef run():\n    X()\n",
 "pkg/X2.py": "",
 "other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "\n"}, {"pkg/__init__.py": "from .m import X\n"}]
SHOW = ["pkg/sub.py"]
