CASES = {
 "A01 default bare call, nested def": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n"},
 "A02 default bare value ref, nested def": {"m.py":
  "def handler():\n    pass\n\n\ndef f(cb=handler):\n    def handler():\n        pass\n    return cb\n"},
 "A03 default ref as call arg, nested def": {"m.py":
  "def handler():\n    pass\n\n\ndef wrap(x):\n    return x\n\n\ndef f(cb=wrap(handler)):\n    def handler():\n        pass\n    return cb\n"},
 "A04 param annotation, nested class": {"m.py":
  "class Row:\n    pass\n\n\ndef f(x: Row):\n    class Row:\n        pass\n    return x\n"},
 "A05 return annotation, nested class": {"m.py":
  "class Row:\n    pass\n\n\ndef f() -> Row:\n    class Row:\n        pass\n    return Row()\n"},
 "A06 PEP695 bound, nested class": {"m.py":
  "class Row:\n    pass\n\n\ndef f[T: Row](x: T):\n    class Row:\n        pass\n    return x\n"},
 "A07 PEP696 type param default, nested class": {"m.py":
  "class Row:\n    pass\n\n\ndef f[T = Row](x: T):\n    class Row:\n        pass\n    return x\n"},
 "A08 lambda in default calls, nested def": {"m.py":
  "def make():\n    pass\n\n\ndef f(cb=lambda: make()):\n    def make():\n        pass\n    return cb\n"},
 "A09 comprehension in default, nested def": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=[make() for _ in range(2)]):\n    def make():\n        pass\n    return x\n"},
 "A10 decorator call, nested def": {"m.py":
  "def deco():\n    return lambda f: f\n\n\n@deco()\ndef g():\n    def deco():\n        pass\n    return 1\n"},
 "A11 decorator bare, nested def": {"m.py":
  "def deco(f):\n    return f\n\n\n@deco\ndef g():\n    def deco():\n        pass\n    return 1\n"},
 "A12 nested def first in a method, default call": {"m.py":
  "def make():\n    pass\n\n\nclass C:\n    def make(self):\n        pass\n    def m(self):\n        def inner(x=make()):\n            pass\n        return inner\n"},
 "A13 nested def default to enclosing fn's def": {"m.py":
  "def make():\n    pass\n\n\ndef outer():\n    def make():\n        pass\n    def f(x=make()):\n        pass\n    return f\n"},
 "A14 method default to class-body method": {"m.py":
  "def make():\n    pass\n\n\nclass C:\n    def make():\n        pass\n    def m(self, x=make()):\n        pass\n"},
 "A15 async def default call, nested def": {"m.py":
  "def make():\n    pass\n\n\nasync def f(x=make()):\n    def make():\n        pass\n    return x\n"},
 "A16 class in fn, method default to fn's def": {"m.py":
  "def make():\n    pass\n\n\ndef outer():\n    def make():\n        pass\n    class C:\n        def m(self, x=make()):\n            pass\n    return C\n"},
 "A17 kw-only default + annotation default, multiline header": {"m.py":
  "def make():\n    pass\n\n\nclass Row:\n    pass\n\n\ndef f(\n    a,\n    *,\n    b: Row = make(),  # comment\n) -> Row:\n    def make():\n        pass\n    class Row:\n        pass\n    return a\n"},
 "A18 nested def (in fn) default, its own body defines": {"m.py":
  "def make():\n    pass\n\n\ndef outer():\n    def f(x=make()):\n        def make():\n            pass\n        return x\n    return f\n"},
 "A19 body call still sees nested def": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=1):\n    def make():\n        pass\n    return make()\n"},
 "A20 one-line def body after header": {"m.py":
  "def make():\n    pass\n\n\ndef outer():\n    def make(): pass\n    def f(x=make()): return make()\n    return f\n"},
 "A21 annotation in nested class header (bases kw)": {"m.py":
  "class Meta(type):\n    pass\n\n\ndef outer():\n    class Meta(type):\n        pass\n    class C(metaclass=Meta):\n        pass\n    return C\n"},
 "A22 default ref when def is first stmt of fn": {"m.py":
  "def handler():\n    pass\n\n\ndef outer():\n    def f(cb=handler):\n        def handler():\n            pass\n        return cb\n    return f\n"},
 "A23 walrus default": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=(y := make())):\n    def make():\n        pass\n    return x\n"},
 "A24 class body, method default after a method of that name": {"m.py":
  "def deco(f):\n    return f\n\n\nclass C:\n    def deco(f):\n        return f\n    @deco\n    def m(self):\n        pass\n"},
 "A25 lambda body nested in default referencing name": {"m.py":
  "def handler():\n    pass\n\n\ndef f(cb=lambda: handler):\n    def handler():\n        pass\n    return cb\n"},
}
