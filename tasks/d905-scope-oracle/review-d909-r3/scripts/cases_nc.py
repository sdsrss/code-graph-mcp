CASES = {
 "NC3 class body global h, name and call in g": {"m.py":
  "def h():\n    pass\n\n\nclass C:\n    global h\n    def h(self):\n        pass\n\n\ndef g():\n    return h\n\n\ndef g2():\n    return h()\n"},
 "NC2 class body name/annotation/decorator before its def, in a fn defining helper": {"m.py":
  "def helper(*a):\n    return a\n\n\ndef F():\n    def helper(*a):\n        return a\n    class K:\n        x = staticmethod(helper)\n        y: helper = helper()\n        @helper\n        def m(self):\n            pass\n        def helper(self):\n            pass\n    return K\n"},
}
