class B:
    def m(self): pass
class C(B):
    def m(self): pass
def f(*, C):
    C.m(None)
