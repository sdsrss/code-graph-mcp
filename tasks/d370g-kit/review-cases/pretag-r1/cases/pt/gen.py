import os, sys
shapes = {
 "aug": "A.m += 0",
 "for": "for A.m in [f]:\n    pass",
 "with": "import contextlib\nwith contextlib.nullcontext(f) as A.m:\n    pass",
 "listpat": "[A.m] = [f]",
 "tuplepat": "(A.m, x) = (f, 1)",
 "annot": "A.m: object = f",
 "chain": "x = A.m = f",
 "star": "*A.m, = [f]",
 "paren2": "((A).m) = f",
 "inner_fn": "def g():\n    A.m = f\ng()",
 "in_class": "class Q:\n    A.m = f",
 "comp": "[None for A.m in [f]]",
}
for k, v in shapes.items():
    d = f"pt_{k}"; os.makedirs(d, exist_ok=True)
    open(f"{d}/a.py", "w").write(f'''class B:
    def m(self):
        return "B.m"


class A(B):
    def m(self):
        return "A.m"


def f(self):
    return "f"


{v}


def t_call():
    return A.m(None)


print(t_call())
''')
