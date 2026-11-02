class B:
    def m(self): pass
class C(B):
    def m(self): pass
del C
C.m(None)
