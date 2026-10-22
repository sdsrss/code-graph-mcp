BEFORE = {
 "pkg/__init__.py": "def X():\n    pass\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "def Y():\n    pass\n"}]
SHOW = ["run@"]
