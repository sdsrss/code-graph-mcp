class B:
    def m(self):
        return "B.m"


class A(B):
    def m(self):
        return "A.m"


delattr(A, "m")


def t_call():
    return A.m(None)


print(t_call())
