class B:
    def m(self): pass
class C(B):
    def m(self): pass
C += 1
C.m(None)
