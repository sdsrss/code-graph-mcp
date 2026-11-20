CASES = {
 "D01 class body keeps every method of that name in file": {"m.py":
  "def helper():\n    pass\n\n\nclass A:\n    def helper(self):\n        pass\n\n\nclass B:\n    x = helper()\n"},
 "D02 NotCovered2: class body LOAD_NAME (def later)": {"m.py":
  "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n        x = staticmethod(helper)\n        def helper(self):\n            pass\n    return C\n"},
 "D03 class body LOAD_NAME (assignment later)": {"m.py":
  "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n        x = staticmethod(helper)\n        helper = None\n    return C\n"},
 "D04 class body LOAD_NAME (nested class later)": {"m.py":
  "class Row:\n    pass\n\n\ndef outer():\n    class Row:\n        pass\n    class C:\n        x = Row()\n        class Row:\n            pass\n    return C\n"},
 "D05 class body LOAD_NAME via method default": {"m.py":
  "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n        def m(self, cb=helper()):\n            pass\n        def helper(self):\n            pass\n    return C\n"},
 "D06 class body LOAD_NAME, call, def later": {"m.py":
  "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n        x = helper()\n        def helper(self):\n            pass\n    return C\n"},
 "D07 class body free name (no class binding)": {"m.py":
  "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n        x = helper()\n    return C\n"},
 "D08 NotCovered1: sees none, two nested": {"m.py":
  "def a():\n    def h():\n        pass\n    return h\n\n\ndef b():\n    def h():\n        pass\n    return h\n\n\ndef c():\n    return h\n"},
 "D09 class body LOAD_NAME, annotation, class later": {"m.py":
  "class Row:\n    pass\n\n\ndef outer():\n    class Row:\n        pass\n    class C:\n        x: Row = None\n        class Row:\n            pass\n    return C\n"},
 "D10 class body LOAD_NAME, decorator, def later": {"m.py":
  "def deco(f):\n    return f\n\n\ndef outer():\n    def deco(f):\n        return f\n    class C:\n        @deco\n        def m(self):\n            pass\n        def deco(self):\n            pass\n    return C\n"},
 "D11 class body LOAD_NAME, comprehension iterable": {"m.py":
  "def helper():\n    return []\n\n\ndef outer():\n    def helper():\n        return []\n    class C:\n        x = [y for y in helper()]\n        def helper(self):\n            pass\n    return C\n"},
}
