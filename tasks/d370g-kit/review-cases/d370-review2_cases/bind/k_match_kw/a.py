class D:
    def m(self):
        return 'D'


class C:
    def m(self):
        return 'C'


class Box:
    __match_args__ = ('v',)
    def __init__(self, v):
        self.v = v
match Box(D):
    case Box(v=C):
        pass


def t_call():
    return C.m(None)
