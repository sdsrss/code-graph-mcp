CASES = {
 "C01 disagreement + from lib import make": {"m.py":
  "from lib import make\n\n\ndef make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n", "lib.py": "def make():\n    pass\n"},
 "C02 disagreement + star import": {"m.py":
  "from lib import *\n\n\ndef make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n", "lib.py": "def make():\n    pass\n"},
 "C03 lambda default": {"m.py":
  "def make():\n    pass\n\n\ndef f(cb=lambda: make()):\n    def make():\n        pass\n    return cb\n"},
 "C04 comprehension default": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=[make() for _ in (1,)]):\n    def make():\n        pass\n    return x\n"},
 "C05 decorator call": {"m.py":
  "def make(*a):\n    return lambda f: f\n\n\n@make()\ndef f():\n    def make():\n        pass\n    return make()\n"},
 "C06 E01 method default": {"m.py":
  "def make():\n    pass\n\n\nclass C:\n    def make(self):\n        pass\n    def m(self, x=make()):\n        def make():\n            pass\n        return x\n"},
 "C07 nested class, sibling call": {"m.py":
  "def Row():\n    pass\n\n\ndef F():\n    class Row:\n        pass\n    def g():\n        return Row()\n    return g()\n"},
 "C08 class body call before method, class in fn": {"m.py":
  "class Row:\n    pass\n\n\ndef F():\n    class Row:\n        pass\n    class K:\n        x = Row()\n        def Row(self):\n            pass\n    return K\n"},
 "C09 global in class body": {"m.py":
  "def h():\n    pass\n\n\nclass C:\n    global h\n    def h(self):\n        pass\n\n\ndef g():\n    return h()\n"},
 "C10 local value call (param namesake)": {"m.py":
  "def make():\n    pass\n\n\ndef p(make=None):\n    return make()\n\n\ndef q():\n    def make():\n        pass\n    return make()\n"},
 "C11 nested def header in fn defining name": {"m.py":
  "def w():\n    def make():\n        pass\n    def inner(x=make()):\n        def make():\n            pass\n        return x\n    return inner\n"},
 "C12 annotation+default, nested class": {"m.py":
  "class Row:\n    pass\n\n\ndef an(x: Row = Row()) -> Row:\n    class Row:\n        pass\n    return Row()\n"},
 "C13 class keyword/base call": {"m.py":
  "def make():\n    return object\n\n\ndef F():\n    class K(make()):\n        def make(self):\n            pass\n    return K\n"},
 "C14 method default, class in fn defining make": {"m.py":
  "def F():\n    def make():\n        pass\n    class K:\n        def m(self, x=make()):\n            def make():\n                pass\n            return x\n    return K\n"},
 "C15 disagreement, call in body too": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return make()\n"},
 "C16 two-file disagreement + lib, use.py calls": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n", "lib.py": "def make():\n    pass\n", "use.py": "from m import make\n\n\ndef u():\n    return make()\n"},
 "C17 module class + method namesake, call in fn": {"m.py":
  "class Row:\n    pass\n\n\nclass K:\n    def Row(self):\n        pass\n\n\ndef g():\n    return Row()\n"},
 "C18 nonlocal class": {"m.py":
  "def outer():\n    class Row:\n        pass\n    def inner():\n        nonlocal Row\n        class Row:\n            pass\n    inner()\n    return Row()\n\n\ndef k():\n    return Row()\n"},
 "C19 header call of a def nested in a method": {"m.py":
  "class C:\n    def make(self):\n        pass\n    def m(self):\n        def inner(x=make()):\n            def make():\n                pass\n            return x\n        return inner\n"},
}
