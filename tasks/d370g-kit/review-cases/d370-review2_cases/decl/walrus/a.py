class B:
    def m(self): pass
class C(B):
    def m(self): pass
if (C := f()):
    C.m(None)
