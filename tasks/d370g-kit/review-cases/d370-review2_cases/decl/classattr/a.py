class B:
    def m(self): pass
class C(B):
    def m(self): pass
class D:
    C = int
C.m(None)
