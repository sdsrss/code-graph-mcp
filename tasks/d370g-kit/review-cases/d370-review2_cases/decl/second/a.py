class B:
    def m(self): pass
class C(B):
    def m(self): pass
class C: pass
C.m(None)
