class X:
    def m(self):
        return "X.m"

    def other(self):
        def m():
            return "inner"
        return m


class Meta(type):
    @property
    def p(cls):
        return lambda *a: "Meta.p"


class B(metaclass=Meta):
    pass


class C(B):
    def p(self):
        return "C.p"


class W:
    def m(self):
        return "W.m"

    @(m := (lambda f: f))
    def z(self):
        pass


class Base2:
    def s(self):
        return "Base2.s"


class Sl(Base2):
    __slots__ = ("s",)


def t_nested():
    return X.m(None)


def t_meta():
    return C.p(None)


def t_walrus():
    return W.m(None)


def t_slots():
    return Sl.s(None)
