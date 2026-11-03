class B:
    def m(self):
        return "B.m"


class NoM(dict):
    def __setitem__(self, k, v):
        if k != "m":
            super().__setitem__(k, v)


class Meta(type):
    @classmethod
    def __prepare__(mcls, name, bases, **kw):
        return NoM()

    def __new__(mcls, name, bases, ns):
        return super().__new__(mcls, name, bases, dict(ns))


class A(B, metaclass=Meta):
    def m(self):
        return "A.m"


def t_call():
    return A.m(None)


print(t_call())
