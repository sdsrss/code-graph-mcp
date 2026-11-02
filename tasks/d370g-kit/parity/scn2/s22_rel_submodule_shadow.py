BEFORE = {
 "pkg/__init__.py": "from .m import X\n",
 "pkg/m.py": "def X():\n    pass\n",
 "pkg/X.py": "def inner():\n    pass\n",
 "pkg/sub.py": "from . import X\n\ndef run():\n    X()\n",
 "other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/X.py": None}, {"pkg/X.py": "def inner():\n    pass\n"}]
SHOW = ["pkg/sub.py"]
