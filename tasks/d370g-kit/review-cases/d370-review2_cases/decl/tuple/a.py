class B:
    def m(self): pass
class C(B):
    def m(self): pass
a, C = x
C.m(None)
