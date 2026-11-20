H = "def h():\n    pass\n\n\n"
F_G = "def f():\n    global h\n    def h():\n        pass\n    return h()\n\n\n"
F_NG = "def f():\n    def h():\n        pass\n    return h()\n\n\n"
G = "def g():\n    return h()\n\n\ndef g2():\n    return h\n"
CASES = {
 "global line removed/restored, call + ref": [
   {"a.py": H + F_G + G},
   {"a.py": H + F_NG + G},
   {"a.py": H + F_G + G},
   {"a.py": "\n\n" + H + F_G + G},  # shift lines
 ],
 "module h moved to b.py and back, global stays": [
   {"a.py": H + F_G + G},
   {"a.py": "from b import h\n\n\n" + F_G + G, "b.py": H},
   {"b.py": None},
   {"a.py": H + F_G + G},
 ],
 "global moved to another file's function": [
   {"a.py": H + F_NG + G, "b.py": "def k():\n    global h\n    h = 1\n"},
   {"a.py": H + F_NG + G + "\n\ndef k():\n    global h\n    h = 1\n", "b.py": None},
   {"a.py": H + F_NG + G},
 ],
 "nonlocal toggled": [
   {"a.py": H + "def outer():\n    def h():\n        pass\n    def inner():\n        nonlocal h\n        def h():\n            pass\n    inner()\n    return h()\n\n\n" + G},
   {"a.py": H + "def outer():\n    def h():\n        pass\n    def inner():\n        def h():\n            pass\n    inner()\n    return h()\n\n\n" + G},
   {"a.py": H + "def outer():\n    def h():\n        pass\n    def inner():\n        nonlocal h\n        def h():\n            pass\n    inner()\n    return h()\n\n\n" + G},
 ],
 "default call module def moved to b.py": [
   {"a.py": "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return make()\n"},
   {"a.py": "from b import make\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return make()\n", "b.py": "def make():\n    pass\n"},
   {"a.py": "def f(x=make()):\n    def make():\n        pass\n    return make()\n"},
   {"b.py": None},
   {"a.py": "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return make()\n"},
 ],
 "nested class renamed, annotation + call, lines shift": [
   {"a.py": "class Row:\n    pass\n\n\ndef f(x: Row = Row()):\n    class Row:\n        pass\n    return Row()\n\n\ndef g(y: Row):\n    return Row()\n"},
   {"a.py": "class Row:\n    pass\n\n\ndef f(x: Row = Row()):\n    class Row2:\n        pass\n    return Row()\n\n\ndef g(y: Row):\n    return Row()\n"},
   {"a.py": "\n\n\nclass Row:\n    pass\n\n\ndef f(x: Row = Row()):\n    class Row:\n        pass\n    return Row()\n\n\ndef g(y: Row):\n    global Row\n    return Row()\n"},
   {"a.py": "def f(x: Row = Row()):\n    class Row:\n        pass\n    return Row()\n\n\ndef g(y: Row):\n    return Row()\n", "b.py": "class Row:\n    pass\n"},
 ],
 "cross-file namesake via star import, global in other file": [
   {"a.py": "from b import *\n\n\n" + F_G + G, "b.py": H},
   {"b.py": "def h():\n    return 1\n\n\ndef k():\n    global h\n"},
   {"b.py": None},
   {"b.py": H},
 ],
 "decorator nested + global": [
   {"a.py": "def deco(f):\n    return f\n\n\n@deco\ndef g():\n    global deco\n    def deco():\n        pass\n    return deco\n\n\ndef u():\n    return deco()\n"},
   {"a.py": "def deco(f):\n    return f\n\n\n@deco\ndef g():\n    def deco():\n        pass\n    return deco\n\n\ndef u():\n    return deco()\n"},
   {"a.py": "@deco\ndef g():\n    def deco():\n        pass\n    return deco\n\n\ndef u():\n    return deco()\n", "c.py": "def deco(f):\n    return f\n"},
 ],
}
