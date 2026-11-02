class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


import sys
sys.modules[__name__].C = D


def t_call():
    return C.m(None)
