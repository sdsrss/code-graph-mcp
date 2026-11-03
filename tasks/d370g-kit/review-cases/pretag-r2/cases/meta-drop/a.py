from meta import Drop


class A:
    def m(self):
        return "A.m"


class B(A, metaclass=Drop):
    def m(self):
        return "B.m"


def run():
    return B.m(B())


print(run())
