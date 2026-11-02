A1 = """class A:
    def m(self):
        return 1

class B(A):
    pass

def f(x):
    return B.m(x)
"""
A2 = """from base import A

class B(A):
    pass

def f(x):
    return B.m(x)
"""
BEFORE = {"a.py": A1, "base.py": "x = 1\n"}
STEPS = [{"a.py": A2, "base.py": "class A:\n    def m(self):\n        return 1\n"}, {"a.py": A1}, {"base.py": None}, {"a.py": A2}]

STEPS = STEPS[:1]
