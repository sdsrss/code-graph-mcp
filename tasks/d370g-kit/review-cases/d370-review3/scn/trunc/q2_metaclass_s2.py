BASE = """class M(type):
    pass

class A{kw}:
    def m(self):
        return 1

class B(A):
    pass

class C(B):
    def m(self):
        return 2

def f(x):
    return B.m(x) + C.m(x)
"""
BEFORE = {"a.py": BASE.format(kw=""), "b.py": "from a import B\n\nclass D(B):\n    def m(self):\n        pass\n"}
STEPS = [{"a.py": BASE.format(kw="(metaclass=M)")}, {"b.py": "class B:\n    def m(self):\n        pass\n"}, {"a.py": BASE.format(kw="")}]

STEPS = STEPS[:2]
