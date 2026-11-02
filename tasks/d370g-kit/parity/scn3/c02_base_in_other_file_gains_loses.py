BASE = "class Remote:\n    def close(self):\n        pass\n"
W = ("from base import Remote\n\nclass Misc:\n    def close(self):\n        pass\n\n"
     "class Local(Remote):\n    def go(self):\n        Local.close(self)\n")
BEFORE = {"base.py": BASE, "w.py": W}
STEPS = [{"base.py": "class Remote:\n    pass\n"}, {"base.py": BASE}, {"base.py": "class Remote2:\n    def close(self):\n        pass\n"}, {"base.py": BASE}]
