class B:
    def _C__m(self):
        return "B._C__m"


class C(B):
    def __m(self):
        return "C.__m"


def t_call():
    return C._C__m(None)


print(t_call())
