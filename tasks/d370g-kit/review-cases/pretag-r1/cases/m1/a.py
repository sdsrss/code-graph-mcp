class B:
    def m(self):
        return "B.m"


class Meta(type):
    def __new__(mcls, name, bases, ns):
        ns.pop("m", None)
        return super().__new__(mcls, name, bases, ns)


class A(B, metaclass=Meta):
    def m(self):
        return "A.m"


def t_call():
    return A.m(None)


print(t_call())
