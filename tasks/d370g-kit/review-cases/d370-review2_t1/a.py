class A:
    def __m(self):
        return "A"

    def run(self):
        return C.__m(self)


class C(A):
    def __m(self):
        return "C"


class X:
    def m(self, n):
        if n:
            return X.m(self, n - 1)


class P:
    def m(self):
        return "P"


class Q:
    def m(self):
        return "Q"


D = P
D.m = Q.m


def use_p():
    return P.m(None)


class S:
    def m(self):
        return "S"


setattr(S, "m", Q.m)


def use_s():
    return S.m(None)
