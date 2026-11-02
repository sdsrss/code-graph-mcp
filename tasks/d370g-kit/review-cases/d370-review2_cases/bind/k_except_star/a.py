class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


try:
    raise ExceptionGroup('g', [ValueError()])
except* ValueError:
    C = D


def t_call():
    return C.m(None)
