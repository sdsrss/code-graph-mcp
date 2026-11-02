class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


E = C
E.m = D.m


def t_call():
    return C.m(None)
