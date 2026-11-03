class B:
    def m(self):
        return "B.m"


class Remover:
    def __set_name__(self, owner, name):
        del owner.m


class A(B):
    def m(self):
        return "A.m"
    x = Remover()


def t_call():
    return A.m(None)


print(t_call())
