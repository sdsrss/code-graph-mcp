class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


for C in [D]:
    pass


def t_call():
    return C.m(None)
