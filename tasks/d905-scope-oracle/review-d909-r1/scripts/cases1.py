CASES = {
    "S01 value ref in a default, inside a call": {"m.py":
        "def handler():\n    pass\n\n\ndef wrap(x):\n    return x\n\n\n"
        "def f(cb=wrap(handler)):\n    def handler():\n        pass\n    return cb\n"},
    "S02 value ref in a default, kw arg of a call": {"m.py":
        "def handler():\n    pass\n\n\n"
        "def f(cb=dict(k=handler)):\n    def handler():\n        pass\n    return cb\n"},
    "S03 value ref in a default, dict value": {"m.py":
        "def handler():\n    pass\n\n\n"
        "def f(cb={'k': handler}):\n    def handler():\n        pass\n    return cb\n"},
    "S04 value ref in a default, list element": {"m.py":
        "def handler():\n    pass\n\n\n"
        "def f(cb=[handler]):\n    def handler():\n        pass\n    return cb\n"},
    "S05 value ref in a default, bare": {"m.py":
        "def handler():\n    pass\n\n\n"
        "def f(cb=handler):\n    def handler():\n        pass\n    return cb\n"},
    "S06 global rebinding nested def (ref)": {"m.py":
        "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\n"
        "def g():\n    return h\n"},
    "S07 global rebinding nested def (call)": {"m.py":
        "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\n"
        "def g():\n    return h()\n"},
    "S08 global rebinding nested class (call)": {"m.py":
        "class Row:\n    pass\n\n\ndef setup():\n    global Row\n    class Row:\n        pass\n\n\n"
        "def use():\n    return Row()\n"},
    "S09 global rebinding nested class (annotation)": {"m.py":
        "class Row:\n    pass\n\n\ndef setup():\n    global Row\n    class Row:\n        pass\n\n\n"
        "def use(x: Row):\n    return x\n"},
    "S10 nonlocal rebinding": {"m.py":
        "def outer():\n    def h():\n        pass\n    def inner():\n        nonlocal h\n        def h():\n            pass\n"
        "    inner()\n    return h\n"},
    "S11 class body local skips enclosing fn (LOAD_NAME)": {"m.py":
        "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n"
        "        x = staticmethod(helper)\n        def helper(self):\n            pass\n    return C\n"},
    "S12 class body free name reads enclosing fn": {"m.py":
        "def helper():\n    pass\n\n\ndef outer():\n    def helper():\n        pass\n    class C:\n"
        "        x = staticmethod(helper)\n    return C\n"},
    "S13 method body skips class scope (nested in fn)": {"m.py":
        "def outer():\n    def helper():\n        pass\n    class C:\n        def helper(self):\n            pass\n"
        "        def m(self):\n            return helper\n    return C\n"},
    "S14 conditional nested defs": {"m.py":
        "def h():\n    pass\n\n\ndef f(flag):\n    if flag:\n        def h():\n            return 1\n    else:\n"
        "        def h():\n            return 2\n    return h\n"},
    "S15 header call to a class the def nests": {"m.py":
        "class Row:\n    pass\n\n\ndef f(x=Row()):\n    class Row:\n        pass\n    return x\n"},
    "S16 header call to a def the def nests (0.185.0 known)": {"m.py":
        "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n"},
    "S17 PEP 695 type param bound": {"m.py":
        "class Row:\n    pass\n\n\ndef f[T: Row](x: T) -> T:\n    class Row:\n        pass\n    return x\n"},
    "S18 PEP 695 type alias in fn": {"m.py":
        "class Row:\n    pass\n\n\ndef f():\n    class Row:\n        pass\n    type A = Row\n    x: A = None\n    return x\n\n\n"
        "def g():\n    class Row:\n        pass\n"},
    "S19 string annotation": {"m.py":
        "def a():\n    class Row:\n        pass\n    x: 'Row' = None\n    return x\n\n\n"
        "def b():\n    class Row:\n        pass\n"},
    "S20 async def header": {"m.py":
        "class Row:\n    pass\n\n\nasync def a(x: Row) -> Row:\n    class Row:\n        pass\n    return x\n"},
    "S21 decorated def header": {"m.py":
        "class Row:\n    pass\n\n\ndef deco(f):\n    return f\n\n\n@deco\ndef a(x: Row) -> Row:\n    class Row:\n        pass\n    return x\n"},
    "S22 first-statement nested def in a method: header ann": {"m.py":
        "class Row:\n    pass\n\n\nclass C:\n    def Row(self):\n        pass\n    def m(self):\n"
        "        def inner(x: Row):\n            pass\n        return inner\n"},
    "S23 non-first nested def in a method: header ann": {"m.py":
        "class Row:\n    pass\n\n\nclass C:\n    def Row(self):\n        pass\n    def m(self):\n"
        "        y = 1\n        def inner(x: Row):\n            pass\n        return inner, y\n"},
    "S24 method header ann, method later (first stmt)": {"m.py":
        "class Row:\n    pass\n\n\nclass C:\n    def m(self, x: Row):\n        pass\n    def Row(self):\n        pass\n"},
    "S25 method header ann, method earlier": {"m.py":
        "class Row:\n    pass\n\n\nclass C:\n    def Row(self):\n        pass\n    def m(self, x: Row):\n        pass\n"},
    "S26 decorator nested vs module": {"m.py":
        "def deco(f):\n    return f\n\n\n@deco\ndef g():\n    def deco():\n        pass\n    return deco\n"},
    "S27 decorator arg in fn": {"m.py":
        "def handler():\n    pass\n\n\ndef register(x):\n    return lambda f: f\n\n\n"
        "def f():\n    def handler():\n        pass\n    @register(handler)\n    def g():\n        pass\n    return g\n"},
    "S28 class keyword arg in fn": {"m.py":
        "class Meta(type):\n    pass\n\n\ndef f():\n    class Meta(type):\n        pass\n    class C(object, metaclass=Meta):\n        pass\n    return C\n"},
    "S29 module class vs method (calls) from function": {"m.py":
        "class time:\n    pass\n\n\nclass datetime:\n    def time(self):\n        pass\n\n\ndef f():\n    return time()\n"},
    "S30 module class vs method from class body": {"m.py":
        "class time:\n    pass\n\n\nclass datetime:\n    def time(self):\n        pass\n    x = time\n    y = staticmethod(time)\n"},
    "S31 isinstance in fn with nested class": {"m.py":
        "class Row:\n    pass\n\n\ndef a(x):\n    class Row:\n        pass\n    return isinstance(x, Row)\n\n\n"
        "def b(x):\n    return isinstance(x, Row)\n"},
    "S32 try/except ImportError nested": {"m.py":
        "def loads(s):\n    pass\n\n\ndef f():\n    try:\n        from json import loads\n    except ImportError:\n"
        "        def loads(s):\n            pass\n    return loads\n\n\ndef g():\n    return loads\n"},
    "S33 star import + nested": {"m.py":
        "from os.path import *\n\n\ndef f():\n    def join(*a):\n        pass\n    return join\n\n\ndef g():\n    return join\n",
        "n.py": "def join(*a):\n    pass\n"},
    "S34 nested class in class referenced from fn": {"m.py":
        "class Outer:\n    class Meta:\n        pass\n\n\ndef f():\n    class Meta:\n        pass\n    x: Meta = None\n    return x\n"},
    "S35 del then module": {"m.py":
        "def h():\n    pass\n\n\ndef f():\n    def h():\n        pass\n    del h\n    return h\n"},
    "S36 lambda default in fn": {"m.py":
        "def handler():\n    pass\n\n\ndef f():\n    def handler():\n        pass\n    g = lambda cb=wrap(handler): cb\n    return g\n\n\ndef wrap(x):\n    return x\n"},
    "S37 overloads": {"m.py":
        "from typing import overload\n\n\nclass Row:\n    pass\n\n\n@overload\ndef f(x: Row) -> Row: ...\n@overload\ndef f(x: int) -> int: ...\n"
        "def f(x):\n    class Row:\n        pass\n    return x\n"},
    "S38 annotation in class body nested in fn": {"m.py":
        "class Row:\n    pass\n\n\ndef f():\n    class Row:\n        pass\n    class C:\n        x: Row\n    return C\n"},
    "S39 class body reads module class, method of same name later": {"m.py":
        "class Row:\n    pass\n\n\nclass C:\n    x: Row\n    def Row(self):\n        pass\n"},
    "S40 kwonly default annotation and value": {"m.py":
        "class Row:\n    pass\n\n\ndef handler():\n    pass\n\n\ndef f(*, x: Row = None, cb=print(handler)):\n    class Row:\n        pass\n    def handler():\n        pass\n    return x, cb\n"},
}
