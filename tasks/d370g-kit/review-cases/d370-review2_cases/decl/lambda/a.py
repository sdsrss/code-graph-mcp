class B:
    def m(self): pass
class C(B):
    def m(self): pass
f = lambda C: C.m(None)
