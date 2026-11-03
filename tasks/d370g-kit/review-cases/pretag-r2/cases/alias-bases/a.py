class A:
    def m(self):
        return "A.m"


class X:
    def m(self):
        return "X.m"


class C(X):
    pass


E = C
E.__bases__ = (A,)


def run():
    return C.m(C())


print(run())
