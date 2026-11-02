BASE = "class Remote:\n    def close(self):\n        pass\n\n    def render(self):\n        pass\n"
W = ("from base import Remote\n\n\nclass Local(Remote):\n    def go(self):\n        Local.close(self)\n        Local.render(self)\n\n\n"
     "class Other:\n    def render(self):\n        pass\n")
X = "class X:\n    def close(self):\n        pass\n\n    def render(self):\n        pass\n"
BEFORE = {"base.py": BASE, "w.py": W, "x.py": X}
STEPS = [{"base.py": "# edit\n" + BASE}, {"base.py": "class Remote:\n    pass\n"}, {"base.py": BASE}, {"base.py": None}, {"base.py": BASE}]
