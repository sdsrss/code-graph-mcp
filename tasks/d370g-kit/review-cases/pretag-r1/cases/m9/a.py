class B:
    def m(self):
        return "B.m"


class Meta(type):
    def __init__(cls, name, bases, ns):
        super().__init__(name, bases, ns)
        if "m" in ns:
            del cls.m


class A(B, metaclass=Meta):
    def m(self):
        return "A.m"


def t_call():
    return A.m(None)


print(t_call())
