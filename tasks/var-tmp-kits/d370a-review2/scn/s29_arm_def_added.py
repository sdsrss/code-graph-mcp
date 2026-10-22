BEFORE = {
 "pkg/__init__.py": "try:\n    from .fast import X\nexcept ImportError:\n    from .slow import X\n",
 "pkg/fast.py": "def Y():\n    pass\n",
 "pkg/slow.py": "def X():\n    pass\n",
 "app/c.py": "import pkg\nfrom pkg import X\n\ndef run():\n    pkg.X()\n    X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/fast.py": "def Y():\n    pass\n\ndef X():\n    pass\n"}]
SHOW = ["app/c.py"]
