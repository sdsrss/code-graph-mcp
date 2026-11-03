class A:
    def m(self):
        return "A.m"


class B:
    def m(self):
        return "B.m"


class C(A):
    pass


C.__bases__ = (B,)


def t_call():
    return C.m(None)


print(t_call())
