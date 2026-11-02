A = ("class A:\n    def m(self):\n        return 1\n\nclass B(A):\n    pass\n\n"
     "def t_b():\n    return B.m(None)\n")
O = "class A:\n    def m(self):\n        return 7\n"
BEFORE = {"a.py": A, "o.py": O}
STEPS = [{"a.py": None, "a2.py": A}, {"a2.py": None, "o.py": None, "a.py": A}, {"o.py": O}]
