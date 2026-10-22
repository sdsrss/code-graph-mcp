import os, sys
d = sys.argv[1]
F = {
 "pkg/__init__.py": "from .core import X\nfrom .core import Klass\n",
 "pkg/core.py": "import app\n\ndef X():\n    return 1\n\nclass Klass:\n    pass\n",
 "pkg2/__init__.py": "from .impl import get\n",
 "pkg2/impl.py": "def get():\n    return 2\n",
 "m.py": "def get():\n    return 3\n\ndef close():\n    return 4\n\ndef used():\n    return 5\n",
 "app.py": "import pkg\nimport pkg2\nimport m\n\ndef f():\n    pkg.X()\n    pkg.Klass()\n    pkg2.get()\n    m.get()\n    m.close()\n    m.used()\n",
 "tests/test_app.py": "import m\nimport pkg\n\ndef test_it():\n    m.used()\n    pkg.X()\n",
}
for p, c in F.items():
    fp = os.path.join(d, p); os.makedirs(os.path.dirname(fp) or d, exist_ok=True); open(fp, "w").write(c)
