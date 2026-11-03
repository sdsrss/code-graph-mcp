import sys


def forget(name):
    sys._getframe(1).f_locals.pop(name)


class A:
    def m(self):
        return "A.m"


class B(A):
    def m(self):
        return "B.m"

    forget("m")


def run():
    return B.m(B())


print(run())
