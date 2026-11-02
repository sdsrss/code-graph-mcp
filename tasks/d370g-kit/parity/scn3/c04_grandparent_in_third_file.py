A = "class Root:\n    def run(self):\n        pass\n"
B = "from a import Root\n\nclass Mid(Root):\n    pass\n"
C = ("from b import Mid\n\nclass Other:\n    def run(self):\n        pass\n\n"
     "class Leaf(Mid):\n    def go(self):\n        Leaf.run(self)\n")
BEFORE = {"a.py": A, "b.py": B, "c.py": C}
STEPS = [{"b.py": B.replace("    pass\n", "    def run(self):\n        pass\n")}, {"b.py": B}, {"a.py": "class Root:\n    pass\n"}, {"a.py": A}]
