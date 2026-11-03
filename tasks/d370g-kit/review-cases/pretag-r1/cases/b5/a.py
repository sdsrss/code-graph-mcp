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


match P(Other):
    case P(D):
        pass


def t_call():
    return D.m(None)


print(t_call())
