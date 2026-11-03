class B:
    def m(self):
        return "B.m"


class A(B):
    def m(self):
        return "A.m"

    def z(self, a=exec("m = B.m")):
        pass


def t_call():
    return A.m(None)


print(t_call())
