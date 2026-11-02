class B:
    def m(self): pass
class C(B):
    def m(self): pass
del a, C
C.m(None)
