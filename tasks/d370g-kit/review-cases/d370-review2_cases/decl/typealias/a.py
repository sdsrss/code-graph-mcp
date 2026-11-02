class B:
    def m(self): pass
class C(B):
    def m(self): pass
type C = int
C.m(None)
