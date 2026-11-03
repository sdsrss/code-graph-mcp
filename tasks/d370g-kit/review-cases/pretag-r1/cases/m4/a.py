class A:
    def m(self):
        return "A.m"


class D:
    def m(self):
        return "D.m"


globals()["A"] = D


def t_call():
    return A.m(None)


print(t_call())
