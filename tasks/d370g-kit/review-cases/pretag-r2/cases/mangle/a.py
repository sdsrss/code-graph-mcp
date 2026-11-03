class A:
    def __m(self):
        return "A.__m"


class C(A):
    def __m(self):
        return "C.__m"

    def other(self):
        return 1


class D(C):
    pass


def caller():
    print(C._C__m(C()))
    print(D._A__m(D()))
    print(D._C__m(D()))


caller()
