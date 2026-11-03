class B:
    def m(self):
        return "B.m"


class A(B):
    def m(self):
        return "A.m"

    z = lambda self, a=exec("m = B.m"): None


def t_call():
    return A.m(None)


print(t_call())
