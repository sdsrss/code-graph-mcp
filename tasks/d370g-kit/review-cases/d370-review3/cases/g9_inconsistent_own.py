class A:
    pass

class B(A):
    pass

class C(A, B):
    def m(self):
        return "C"

def call(x):
    return C.m(x)
