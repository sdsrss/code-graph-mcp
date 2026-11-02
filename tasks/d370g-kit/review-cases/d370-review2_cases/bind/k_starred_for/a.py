class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


for *C, in [[D]]:
    C = C[0]


def t_call():
    return C.m(None)
