W = "class Base:\n    pass\n\n\nclass C(Base):\n    def f(self):\n        C.render(self)\n        C.close(self)\n"
O = "class Base:\n    def render(self):\n        pass\n\n    def close(self):\n        pass\n"
BEFORE = {"w.py": W, "x.py": "def h():\n    pass\n"}
STEPS = [{"o.py": O}, {"o.py": None}, {"o.py": O}]
