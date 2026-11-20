CASES = {
 "H1 default call, no module def, nested namesakes elsewhere": {"m.py":
  "def f(x=make()):\n    def make():\n        pass\n    return x\n\n\ndef g():\n    def make():\n        pass\n    return make()\n"},
 "H2 same + imported make": {"m.py":
  "from lib import make\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n\n\ndef g():\n    def make():\n        pass\n    return make()\n",
  "lib.py": "def make():\n    pass\n"},
 "H3 nested def's header in a method, no module def": {"m.py":
  "class C:\n    def m(self, x=make()):\n        def make():\n            pass\n        return x\n\n\ndef g():\n    def make():\n        pass\n    return make()\n"},
 "H4 default call, def has no nested make, only other fns do": {"m.py":
  "def f(x=make()):\n    return x\n\n\ndef g():\n    def make():\n        pass\n    return make()\n"},
 "H5 star import + nested namesakes": {"m.py":
  "from lib import *\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n\n\ndef g():\n    def make():\n        pass\n    return make()\n",
  "lib.py": "def make():\n    pass\n"},
 "H6 default call, the nested def of f only (no other)": {"m.py":
  "def f(x=make()):\n    def make():\n        pass\n    return x\n"},
}
