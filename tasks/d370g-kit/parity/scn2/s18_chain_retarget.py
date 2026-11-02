BEFORE = {
 "pkg/__init__.py": "from .sub import X\n",
 "pkg/sub/__init__.py": "from .impl import X\n",
 "pkg/sub/impl.py": "def X():\n    pass\n",
 "pkg/sub/other.py": "def W():\n    pass\n",
 "app/user.py": "import pkg\nfrom pkg import X\n\ndef run():\n    pkg.X()\n    X()\n",
 "app/decoy.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/sub/impl.py": "def V():\n    pass\n", "pkg/sub/other.py": "def X():\n    pass\n", "pkg/sub/__init__.py": "from .other import X\n"}]
SHOW = ["app/user.py"]
