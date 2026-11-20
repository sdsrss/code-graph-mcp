CASES = {
 "F01 match class pattern in body, nested class, module class": {"m.py":
  "class Row:\n    pass\n\n\ndef f(v, d=Row()):\n    class Row:\n        pass\n    match v:\n        case Row():\n            return Row()\n"},
 "F02 conditional redefined nested defs + global in one branch": {"m.py":
  "def h():\n    pass\n\n\ndef f(c):\n    if c:\n        global h\n        def h():\n            pass\n    else:\n        def h():\n            pass\n    return h()\n"},
 "F03 nested async def named in nonlocal, closure call": {"m.py":
  "async def h():\n    pass\n\n\ndef outer():\n    async def h():\n        pass\n    def inner():\n        nonlocal h\n        async def h():\n            pass\n    inner()\n    return h()\n"},
 "F04 PEP695 class type param bound in fn": {"m.py":
  "class Row:\n    pass\n\n\ndef outer():\n    class Row:\n        pass\n    class C[T: Row]:\n        pass\n    return C\n"},
 "F05 decorated nested def whose decorator is defined in the decorated body (nested)": {"m.py":
  "def deco(f):\n    return f\n\n\ndef outer():\n    def deco(f):\n        return f\n    @deco\n    def g():\n        def deco():\n            pass\n        return deco\n    return g\n"},
}
