A1 = ("class X:\n    def m(self, n):\n        if n:\n            return X.m(self, n - 1)\n\n"
      "class Y(X):\n    def m(self, n):\n        return X.m(self, n)\n")
B1 = "class X:\n    def m(self, n):\n        return n\n"
BEFORE = {"a.py": A1, "b.py": B1}
STEPS = [{"b.py": "class X:\n    def k(self, n):\n        return n\n"}, {"b.py": None}, {"b.py": B1},
         {"a.py": A1 + "\n# edit\n"}, {"c.py": "class X(object):\n    def m(self, n):\n        return 0\n"}]
