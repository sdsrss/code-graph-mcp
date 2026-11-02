class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


import contextlib
with contextlib.nullcontext(D) as C:
    pass


def t_call():
    return C.m(None)
