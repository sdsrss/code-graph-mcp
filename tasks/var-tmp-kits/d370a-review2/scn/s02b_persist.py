# module's X moves from nested (in a function) to top level: name count unchanged
BEFORE = {
 "pkg/__init__.py": "def outer():\n    def X():\n        pass\n    return X\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "def outer():\n    return 1\n\ndef X():\n    pass\n"}, {"zz_unrelated.py": "def q():\n    pass\n"}, {"zz_unrelated.py": "def q():\n    return 2\n"}]
SHOW = ["run@"]
