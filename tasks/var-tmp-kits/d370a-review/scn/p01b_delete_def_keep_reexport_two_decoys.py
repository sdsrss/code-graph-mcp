BEFORE = {
 "pkg/__init__.py": "try:\n    from ._speed import X\nexcept ImportError:\n    def X():\n        pass\n",
 "pkg/_speed.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "yyy/decoy2.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
}
STEPS = [{"pkg/__init__.py": "from ._speed import X\n"}]
SHOW = ["app.py:main"]
