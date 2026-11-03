class B:
    def m(self):
        return "B.m"


def inject(ns):
    ns["m"] = B.m
    return lambda f: f


class A(B):
    def m(self):
        return "A.m"

    @inject(locals())
    def z(self):
        pass


def t_call():
    return A.m(None)


print(t_call())
