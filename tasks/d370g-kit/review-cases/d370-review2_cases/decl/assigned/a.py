class B:
    def m(self): pass
class C(B):
    def m(self): pass
C.m = f
C.m(None)
