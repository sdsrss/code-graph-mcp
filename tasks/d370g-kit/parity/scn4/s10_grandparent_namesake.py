W = "from mid import Mid\n\n\nclass C(Mid):\n    def f(self):\n        C.render(self)\n        C.close(self)\n"
MID = "from top import Top\n\n\nclass Mid(Top):\n    pass\n"
TOP = "class Top:\n    def render(self):\n        pass\n\n    def close(self):\n        pass\n"
BEFORE = {"w.py": W, "mid.py": MID, "top.py": TOP}
STEPS = [{"other.py": "class Top:\n    def render(self):\n        pass\n"}, {"top.py": None}, {"other.py": None}, {"top.py": TOP}]
