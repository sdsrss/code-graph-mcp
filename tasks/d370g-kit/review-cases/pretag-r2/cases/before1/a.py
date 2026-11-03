from ext import *


def f(a=C.m()):
    return a


g = lambda a=C.m(): a


class Other:
    def h(self, a=C.m()):
        return a


class C:
    x = C.m()

    def m(self=None):
        return "a.C.m"


print(f(), g(), Other().h(), C.x)
