A = ("class A:\n    def m(self):\n        return 1\n\nclass B(A):\n    pass\n\n"
     "def t_b():\n    return B.m(None)\n\ndef t_a():\n    return A.m(None)\n")
BEFORE = {"a.py": A, "z.py": "def f():\n    pass\n"}
STEPS = [{"b.py": "class B:\n    def m(self):\n        return 2\n\nclass A:\n    def m(self):\n        return 3\n"},
         {"b.py": "from a import A\n\nclass B(A):\n    def m(self):\n        return 4\n"},
         {"c.py": "from a import B\n\nclass Sub(B):\n    def m(self):\n        return 5\n"},
         {"b.py": None}, {"c.py": None}]
