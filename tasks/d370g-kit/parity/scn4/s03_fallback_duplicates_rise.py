W = "class E(Exception):\n    def f(self):\n        E.render(self)\n        E.close(self)\n"
BEFORE = {"w.py": W, "y.py": "class Y:\n    def render(self):\n        pass\n"}
STEPS = [{"z.py": "class Z:\n    def render(self):\n        pass\n\n    def close(self):\n        pass\n"}, {"y.py": None}, {"z.py": None}, {"y.py": "class Y:\n    def render(self):\n        pass\n"}]
