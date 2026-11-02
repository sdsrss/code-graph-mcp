A1 = ("class A:\n    def m(self):\n        return 1\n\nclass B(A):\n    pass\n\n"
      "def t_b():\n    return B.m(None)\n")
A2 = A1 + "\nB = A\n"
A3 = A1.replace("class B(A):\n    pass", "class B(A):\n    def m(self):\n        return 9")
BEFORE = {"a.py": A1, "b.py": "class B:\n    def m(self):\n        return 2\n\nclass A:\n    def m(self):\n        return 3\n"}
STEPS = [{"a.py": A2}, {"a.py": A1}, {"a.py": A3, "b.py": "class B(object):\n    def m(self):\n        return 2\n"}, {"a.py": A1}]
