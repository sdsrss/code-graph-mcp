CASES = {
 "B19 global h + nested def h + call in f, no module h, method h": {"m.py":
  "class C:\n    def h(self):\n        pass\n\n\ndef f():\n    global h\n    def h():\n        pass\n    return h()\n"},
 "B20 other fn's global unscopes g's nested h; method h; no module h": {"m.py":
  "class C:\n    def h(self):\n        pass\n\n\ndef k():\n    global h\n    h = None\n\n\ndef g():\n    def h():\n        pass\n    return h()\n"},
 "B21 global h in f, two other fns each define local h, call in a third": {"m.py":
  "def h():\n    pass\n\n\ndef f():\n    global h\n    h = 1\n\n\ndef a():\n    def h():\n        pass\n    return h()\n\n\ndef b():\n    def h():\n        pass\n    return h()\n"},
}
