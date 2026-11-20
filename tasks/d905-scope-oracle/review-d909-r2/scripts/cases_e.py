CASES = {
 "E01 method default, class body defines make before, m nests make": {"m.py":
  "def make():\n    pass\n\n\nclass C:\n    def make(self):\n        pass\n    def m(self, x=make()):\n        def make():\n            pass\n        return x\n"},
 "E02 method default, class body defines make after, m nests make": {"m.py":
  "def make():\n    pass\n\n\nclass C:\n    def m(self, x=make()):\n        def make():\n            pass\n        return x\n    def make(self):\n        pass\n"},
 "E03 nested def default, enclosing fn and own body define make": {"m.py":
  "def make():\n    pass\n\n\ndef outer():\n    def make():\n        pass\n    def f(x=make()):\n        def make():\n            pass\n        return x\n    return f\n"},
 "E04 class body call in nested class header (bases call)": {"m.py":
  "def mk():\n    return object\n\n\ndef outer():\n    def mk():\n        return object\n    class C(mk()):\n        pass\n    return C\n"},
 "E05 lambda default inside class body method header": {"m.py":
  "def make():\n    pass\n\n\nclass C:\n    def make(self):\n        pass\n    def m(self, cb=lambda: make()):\n        def make():\n            pass\n        return cb\n"},
}
