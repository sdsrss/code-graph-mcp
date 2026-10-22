BEFORE = {
 "pkg/__init__.py": "def X():\n    pass\n\ntry:\n    from ._fast import X\nexcept ImportError:\n    pass\n",
 "pkg/_fast.py": "def X():\n    pass\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/_fast.py": None}]
SHOW = ["app/c.py"]
