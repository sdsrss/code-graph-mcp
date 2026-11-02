class Q:
    def m(self):
        return "Q"


class C:
    def m(self):
        return "C"

    def z(self, a=(m := Q.m)):
        return a


class E:
    def m(self):
        return "E"

    locals()["m"] = Q.m


def t_default():
    return C.m(None)


def t_locals():
    return E.m(None)
