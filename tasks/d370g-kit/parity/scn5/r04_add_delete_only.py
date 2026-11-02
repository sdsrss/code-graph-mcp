A = ("class A:\n    def m(self):\n        return 1\n\nclass B(A):\n    pass\n\n"
     "def t_b():\n    return B.m(None)\n\nclass C:\n    def m(self):\n        return C.m(self)\n")
BEFORE = {"a.py": A}
STEPS = [{"c.py": "class A:\n    def m(self):\n        return 3\n\nclass B(A):\n    pass\n\nclass C:\n    def m(self):\n        pass\n"},
         {"d.py": "class B:\n    def m(self):\n        return 4\n"}, {"c.py": None}, {"d.py": None},
         {"e.py": "from a import *\n\nclass D(B):\n    pass\n\ndef t_e():\n    return D.m(None)\n"}]
