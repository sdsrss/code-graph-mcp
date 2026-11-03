from ext import *

g = lambda a=C.m(): a


class C:
    def m(self=None):
        return "a.C.m"


print(g())
