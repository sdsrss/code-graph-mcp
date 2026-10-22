# fallback by name had two candidates in other files
BEFORE = {
 "pkg/__init__.py": "VERSION = 1\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.X()\n",
 "lib1/a.py": "def X():\n    pass\n",
 "lib2/b.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "VERSION = 1\n\ndef X():\n    pass\n"}]
SHOW = ["run@"]
