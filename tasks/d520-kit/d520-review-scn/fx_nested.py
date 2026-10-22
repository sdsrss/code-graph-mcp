FILES = {
 "m.py": "def X():\n    pass\n\nclass Outer:\n    class Inner:\n        pass\n    def meth(self):\n        pass\n\ndef make():\n    class Local:\n        pass\n    def helper():\n        pass\n    return Local\n\ndef helper():\n    pass\n",
 "q.py": "def X():\n    pass\n\nclass Inner:\n    pass\n\ndef meth():\n    pass\n\nclass Local:\n    pass\n",
 "c2.py": "import m\n\ndef f():\n    m.Inner()\n    m.Local()\n    m.meth()\n",
 "c3.py": "import m\n\ndef f():\n    m.helper()\n",
}
SHOW = ["c2.py", "c3.py"]
