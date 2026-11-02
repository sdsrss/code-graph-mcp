# attr caller bound to pkg's own def; pkg gains a class with a same-named method
BEFORE = {
 "pkg/__init__.py": "def X():\n    pass\n",
 "zzz/decoy.py": "def X():\n    pass\n",
 "app.py": "import pkg\n\ndef main():\n    pkg.X()\n",
}
STEPS = [{"pkg/__init__.py": "def X():\n    pass\n\nclass C:\n    def X(self):\n        pass\n"}]
SHOW = ["app"]
