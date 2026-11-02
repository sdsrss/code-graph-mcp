class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


[(C := D) for _ in [1]]


def t_call():
    return C.m(None)
