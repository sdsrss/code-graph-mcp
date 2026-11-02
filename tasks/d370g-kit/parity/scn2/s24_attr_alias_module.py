BEFORE = {
 "a/__init__.py": "",
 "a/b/__init__.py": "from .c import X\n",
 "a/b/c.py": "def X():\n    pass\n",
 "a/b/d.py": "def Q():\n    pass\n",
 "app/u.py": "import a.b as m\n\ndef run():\n    m.X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"a/b/__init__.py": "from .d import X\n", "a/b/d.py": "def X():\n    pass\n"}, {"a/b/c.py": None}]
SHOW = ["app/u.py"]
