class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


def f():
    global C
    C = D
f()


def t_call():
    return C.m(None)
