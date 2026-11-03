from ext import *


def t_call():
    return A.m(None)


print(t_call())


class A:
    def m(self):
        return "a.A.m"
