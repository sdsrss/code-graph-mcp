from ext import *


class C:
    x = C.m()

    def m(self=None):
        return "a.C.m"


print(C.x)
