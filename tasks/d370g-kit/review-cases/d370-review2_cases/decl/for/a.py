class B:
    def m(self): pass
class C(B):
    def m(self): pass
for C in xs:
    C.m(None)
