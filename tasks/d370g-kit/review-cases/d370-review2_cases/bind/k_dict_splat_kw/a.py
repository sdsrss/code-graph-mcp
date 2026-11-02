class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


def g(**kw):
    return kw
g(**{'C': 1})


def t_call():
    return C.m(None)
