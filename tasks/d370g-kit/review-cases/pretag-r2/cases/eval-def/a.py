class A:
    def eval(self):
        return 0

    def m(self):
        return "A.m"


class B:
    def m(self):
        return "B.m"


class C(A):
    def run(self, vars=None):
        return C.m(self)


print(C().run())
