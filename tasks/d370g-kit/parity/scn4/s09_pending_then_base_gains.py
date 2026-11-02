W = "from base import Remote\n\n\nclass Local(Remote):\n    def go(self):\n        Local.render(self)\n        Local.close(self)\n"
BEFORE = {"base.py": "class Remote:\n    pass\n", "w.py": W}
STEPS = [{"x.py": "class X:\n    def render(self):\n        pass\n\n    def close(self):\n        pass\n"},
         {"base.py": "class Remote:\n    def render(self):\n        pass\n\n    def close(self):\n        pass\n"},
         {"x.py": None}, {"base.py": "class Remote:\n    pass\n"}]
