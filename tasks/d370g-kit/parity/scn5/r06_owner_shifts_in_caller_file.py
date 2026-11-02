A1 = ("class Base:\n    def m(self):\n        return 0\n\nclass A(Base):\n    def m(self):\n        return 1\n\nclass B(A):\n    pass\n\n"
      "def t_b():\n    return B.m(None)\n")
A2 = A1.replace("class A(Base):\n    def m(self):\n        return 1", "class A(Base):\n    pass")
O = "class A:\n    def m(self):\n        return 7\n\nclass Base:\n    def m(self):\n        return 8\n"
BEFORE = {"a.py": A1, "o.py": O}
STEPS = [{"a.py": A2}, {"o.py": O + "\nclass B(A):\n    def m(self):\n        return 9\n"}, {"a.py": A1}, {"o.py": None}]
