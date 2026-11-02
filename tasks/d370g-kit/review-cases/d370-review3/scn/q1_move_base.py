A = """class A:
    def m(self):
        return 1

class B(A):
    pass

def f(x):
    return B.m(x)
"""
A2 = """class B(A):
    pass

class A:
    def m(self):
        return 1

def f(x):
    return B.m(x)
"""
OTHER = "from a import A\n\nclass A2:\n    def m(self):\n        pass\n\ndef g(x):\n    return A2.m(x)\n"
BEFORE = {"a.py": A, "o.py": OTHER}
STEPS = [{"a.py": A2}, {"a.py": A}, {"o.py": OTHER + "\nclass B:\n    def m(self):\n        pass\n"}]
