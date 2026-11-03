class B:
    def m(self, n=0):
        return "B.m"


class A(B):
    def m(self, n=0):
        if n:
            return A.m(self, n - 1)
        return "A.m"


print(A.m(A(), 2))
