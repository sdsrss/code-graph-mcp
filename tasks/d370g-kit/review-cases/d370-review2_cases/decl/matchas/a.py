class B:
    def m(self): pass
class C(B):
    def m(self): pass
match x:
    case D() as C:
        C.m(None)
