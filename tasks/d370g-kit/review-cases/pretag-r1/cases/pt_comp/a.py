class B:
    def m(self):
        return "B.m"


class A(B):
    def m(self):
        return "A.m"


def f(self):
    return "f"


[None for A.m in [f]]


def t_call():
    return A.m(None)


print(t_call())
