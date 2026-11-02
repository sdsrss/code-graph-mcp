class B:
    def m(self): pass
class C(B):
    def m(self): pass
match x:
    case C:
        C.m(None)
