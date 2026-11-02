class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


setattr(C, 'm', D.m)


def t_call():
    return C.m(None)
