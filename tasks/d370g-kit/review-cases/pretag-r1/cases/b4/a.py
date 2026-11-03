class P:
    __match_args__ = ("x",)
    def __init__(self, x=None):
        self.x = x


class D:
    def m(self):
        return "D.m"


class Other:
    def m(self):
        return "Other.m"


def bind(v):
    global D
    match {"k": v.x}:
        case {"k": D}:
            pass


def t_call():
    return D.m(None)


bind(P(Other))
print(t_call())
