CASES = {
 "C01 unclosed paren mid-body": {"m.py":
  "def make():\n    pass\n\n\ndef f():\n    def make():\n        pass\n    x = (1,\n    return make()\n\n\ndef g():\n    return make()\n"},
 "C02 py2 print chevron in body": {"m.py":
  "def make():\n    pass\n\n\ndef f(out):\n    def make():\n        pass\n    print >>out, 'x'\n    exec 'y = 1'\n    return make()\n"},
 "C03 stray dedent junk": {"m.py":
  "def make():\n    pass\n\n\ndef f():\n    def make():\n        pass\n    if x\n        y = 1\n    return make()\n"},
 "C04 backticks py2": {"m.py":
  "def make():\n    pass\n\n\ndef f():\n    def make():\n        pass\n    s = `make`\n    return make()\n"},
 "C05 ur-string and <> py2": {"m.py":
  "def make():\n    pass\n\n\ndef f(a, b):\n    def make():\n        pass\n    if a <> b:\n        return make()\n    return make()\n"},
 "C06 broken header": {"m.py":
  "def make():\n    pass\n\n\ndef f(x=, y=make()):\n    def make():\n        pass\n    return make()\n"},
 "C07 except comma py2": {"m.py":
  "def make():\n    pass\n\n\ndef f():\n    def make():\n        pass\n    try:\n        pass\n    except Exception, e:\n        return make()\n    return make()\n"},
 "C08 0777 octal py2": {"m.py":
  "def make():\n    pass\n\n\ndef f():\n    def make():\n        pass\n    os.chmod(p, 0777)\n    return make()\n"},
}
