class B:
    def m(self): pass
class C(B):
    def m(self): pass
C = wrap(C)
C.m(None)
