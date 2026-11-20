CASES = {
 "B01 global h in f + def h; call in g (claimed)": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "B02 global h in f + def h; call in f after def": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n    return h()\n"},
 "B03 global h in f; call in f before def": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    global h\n    h()\n    def h():\n        pass\n"},
 "B04 global h elsewhere (k assigns); local def h in g": {"m.py":
  "def h():\n    pass\n\n\ndef k():\n    global h\n    h = None\n\n\ndef g():\n    def h():\n        pass\n    return h()\n\n\ndef m():\n    return h()\n"},
 "B04r same, name refs": {"m.py":
  "def h():\n    pass\n\n\ndef k():\n    global h\n    h = None\n\n\ndef g():\n    def h():\n        pass\n    return h\n\n\ndef m():\n    return h\n"},
 "B05 nonlocal h in inner (def h), call in unrelated fn": {"m.py":
  "def h():\n    pass\n\n\ndef outer():\n    def h():\n        pass\n    def inner():\n        nonlocal h\n        def h():\n            pass\n    inner()\n    return h()\n\n\ndef k():\n    return h()\n"},
 "B06 global a, h (two names)": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    global a, h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "B07 global h at module level": {"m.py":
  "global h\n\n\ndef h():\n    pass\n\n\ndef g():\n    def h():\n        pass\n    return h()\n"},
 "B08 nonlocal h in a class body": {"m.py":
  "def outer():\n    def h():\n        pass\n    class C:\n        nonlocal h\n        def h(self):\n            pass\n    return h()\n"},
 "B09 global in g's inner fn, g has local h": {"m.py":
  "def h():\n    pass\n\n\ndef g():\n    def h():\n        pass\n    def inner():\n        global h\n        return h()\n    return h()\n"},
 "B10 global h; class h": {"m.py":
  "class h:\n    pass\n\n\ndef f():\n    global h\n    class h:\n        pass\n\n\ndef g(x: h):\n    return h()\n"},
 "B11 global in class body": {"m.py":
  "def h():\n    pass\n\n\nclass C:\n    global h\n    def h(self):\n        pass\n\n\ndef g():\n    return h()\n"},
 "B12 global h, method h, nested def h in f": {"m.py":
  "def h():\n    pass\n\n\nclass C:\n    def h(self):\n        pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "B13 global h in f, no module h, call in g": {"m.py":
  "def f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "B14 global in nested of f, def h in nested": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    def inner():\n        global h\n        def h():\n            pass\n    return h()\n"},
 "B15 nonlocal, call in outer after inner()": {"m.py":
  "def outer():\n    def h():\n        pass\n    def inner():\n        nonlocal h\n        def h():\n            pass\n    inner()\n    return h()\n"},
 "B16 global stmt name with parenthesis/semicolon": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    x = 1; global h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "B17 global decl in string not code": {"m.py":
  "def h():\n    pass\n\n\nS = 'global h'\n\n\ndef g():\n    def h():\n        pass\n    return h()\n"},
 "B18 global in default lambda? (decorated nested)": {"m.py":
  "def h():\n    pass\n\n\ndef deco(fn):\n    return fn\n\n\ndef f():\n    global h\n    @deco\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
}
