CASES = {
 "C-B02": {"m.py": "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n    return h()\n"},
 "C-B04": {"m.py": "def h():\n    pass\n\n\ndef k():\n    global h\n    h = None\n\n\ndef g():\n    def h():\n        pass\n    return h()\n\n\ndef m():\n    return h()\n"},
 "C-H1": {"m.py": "def f(x=make()):\n    def make():\n        pass\n    return x\n\n\ndef g():\n    def make():\n        pass\n    return make()\n"},
 "C-E01": {"m.py": "def make():\n    pass\n\n\nclass C:\n    def make(self):\n        pass\n    def m(self, x=make()):\n        def make():\n            pass\n        return x\n"},
 "C-JS": {"m.js": "function make() {}\n\nfunction f(x = make()) {\n  function make() {}\n  return x;\n}\n"},
 "C-A01-2files": {"m.py": "def make():\n    pass\n\n\ndef f(x=make()):\n    def make():\n        pass\n    return x\n", "lib.py": "def make():\n    pass\n"},
 "R-X08": {"m.py": "def h():\n    pass\n\n\ndef f():\n    global a, h\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
 "R-X09": {"m.py": "def h():\n    pass\n\n\nclass C:\n    def h(self):\n        pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
 "R-X10": {"m.py": "def h():\n    pass\n\n\ndef k():\n    global zz\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h\n"},
}
