class B:
    def _C__m(self):
        return "B._C__m"


class C(B):
    def __m(self):
        return "C.__m"


class A:
    def run(self):
        return C._C__m(self)


print(A().run())
