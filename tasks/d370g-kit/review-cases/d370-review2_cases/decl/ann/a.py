class B:
    def m(self): pass
class C(B):
    def m(self): pass
C: type = X
C.m(None)
