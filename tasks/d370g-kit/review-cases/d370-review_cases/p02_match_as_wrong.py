FILES = {"shapes.py": """class C:
    def m(self):
        pass


class D:
    def m(self):
        pass


def f(x):
    match x:
        case D() as C:
            C.m()
"""}
