run = exec


class A:
    def m(self):
        return "A.m"


class B(A):
    def m(self):
        return "B.m"

    run("del m")


def call():
    return B.m(B())


print(call())
