# module's X moves from a method to top level: name count unchanged
BEFORE = {
 "pkg/__init__.py": "class K:\n    def X(self):\n        pass\n",
 "app/c.py": "import pkg\n\ndef run():\n    pkg.X()\n",
 "app/other.py": "def X():\n    pass\n",
}
STEPS = [{"pkg/__init__.py": "class K:\n    pass\n\ndef X():\n    pass\n"}]
SHOW = ["run@"]
