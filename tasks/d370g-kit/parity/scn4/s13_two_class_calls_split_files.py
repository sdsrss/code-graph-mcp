A = "class A:\n    def render(self):\n        pass\n"
B = "class B:\n    def render(self):\n        pass\n"
W = ("from a import A\nfrom b import B\n\n\nclass C(A):\n    pass\n\n\nclass D(B):\n    pass\n\n\n"
     "def f(x):\n    C.render(x)\n    D.render(x)\n")
BEFORE = {"a.py": A, "b.py": B, "w.py": W}
STEPS = [{"a.py": "# t\n" + A}, {"b.py": "# t\n" + B}, {"a.py": "# t2\n" + A, "b.py": "# t2\n" + B}]
