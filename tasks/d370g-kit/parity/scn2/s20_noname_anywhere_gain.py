BEFORE = {
 "pkg/__init__.py": "VERSION = 1\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.Xyz()\n",
}
STEPS = [{"pkg/__init__.py": "VERSION = 1\n\ndef Xyz():\n    pass\n"}]
SHOW = ["run@"]
