class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


(C := D)


def t_call():
    return C.m(None)
