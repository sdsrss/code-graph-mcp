class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


x = C = D


def t_call():
    return C.m(None)
