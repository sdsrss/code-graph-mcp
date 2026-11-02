SRC = """class A:
    def m(self):
        return 1

class B:
    def m(self):
        return 2

class D({bases}):
    pass
{patch}
def f(x):
    return D.m(x)
"""
BEFORE = {"a.py": SRC.format(bases="A, B", patch="")}
STEPS = [{"a.py": SRC.format(bases="B, A", patch="")}, {"a.py": SRC.format(bases="B, A", patch="(B).m = len\n")},
         {"a.py": SRC.format(bases="B, A", patch="del (B).m\n")}, {"a.py": SRC.format(bases="A, B", patch="")},
         {"z.py": "from a import D\nD.m = print\n"}]
