class B:
    def m(self):
        return "B.m"


class A(B):
    def m(self):
        return "A.m"


def f(self):
    return "f"


def g():
    A.m = f
g()


def t_call():
    return A.m(None)


print(t_call())
