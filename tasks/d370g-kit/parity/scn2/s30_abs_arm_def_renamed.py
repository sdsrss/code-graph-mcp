BEFORE = {
 "pkg/__init__.py": "try:\n    from pkg.fast import X\nexcept ImportError:\n    from pkg.slow import X\n",
 "pkg/fast.py": "def X():\n    pass\n",
 "pkg/slow.py": "def X():\n    pass\n",
 "app/c.py": "import pkg\nfrom pkg import X\n\ndef run():\n    pkg.X()\n    X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/fast.py": "def Y():\n    pass\n"}]
SHOW = ["app/c.py"]
