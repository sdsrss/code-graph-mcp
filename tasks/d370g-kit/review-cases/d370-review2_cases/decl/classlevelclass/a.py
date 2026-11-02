class B:
    def m(self): pass
class C(B):
    def m(self): pass
class D:
    class C:
        def m(self): pass
C.m(None)
