CASES = {
 "X03 js default + nested fn (M03)": {"m.js":
  "function make() {}\n\nfunction f(x = make()) {\n  function make() {}\n  return x;\n}\n"},
 "X08 global a, h (M08)": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    global a, h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "X09 module h + method h + global nested h (M09)": {"m.py":
  "def h():\n    pass\n\n\nclass C:\n    def h(self):\n        pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "X10 two global statements (M10)": {"m.py":
  "def h():\n    pass\n\n\ndef k():\n    global zz\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
 "X14 module-level global vs nested (M14)": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n\n\ndef g():\n    return h()\n"},
}
