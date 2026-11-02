# module gains top-level def X; caller's attr call had fallen back to its own same-file X
BEFORE = {
 "pkg/__init__.py": "VERSION = 1\n",
 "app/c.py": "import pkg\n\ndef X():\n    pass\n\ndef run():\n    pkg.X()\n",
}
STEPS = [{"pkg/__init__.py": "VERSION = 1\n\ndef X():\n    pass\n"}]
SHOW = ["run@"]
