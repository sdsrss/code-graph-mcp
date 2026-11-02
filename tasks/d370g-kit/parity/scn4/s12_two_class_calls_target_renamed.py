BASE = "class A:\n    def render(self):\n        pass\n\n\nclass B:\n    def render(self):\n        pass\n"
W = ("from base import A, B\n\n\nclass C(A):\n    pass\n\n\nclass D(B):\n    pass\n\n\n"
     "def f(x):\n    C.render(x)\n    D.render(x)\n")
BEFORE = {"base.py": BASE, "w.py": W}
STEPS = [{"base.py": BASE.replace("class B:\n    def render", "class B:\n    def draw")}, {"base.py": BASE}]
