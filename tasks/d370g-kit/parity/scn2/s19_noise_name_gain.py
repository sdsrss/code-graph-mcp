BEFORE = {
 "pkg/__init__.py": "VERSION = 1\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.build()\n",
 "app/other.py": "def build():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "VERSION = 1\n\ndef build():\n    pass\n"}]
SHOW = ["run@"]
