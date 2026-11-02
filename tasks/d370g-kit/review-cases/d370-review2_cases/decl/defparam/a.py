class B:
    def m(self): pass
class C(B):
    def m(self): pass
def f(C=1):
    C.m(None)
