import types

x = types.SimpleNamespace(C=1).C


class C:
    def m(self):
        return "C.m"


class D:
    def m(self):
        return "D.m"


def t():
    return C.m(None)


print(t())
