class B:
    def m(self): pass
class C(B):
    def m(self): pass
y = [C.m(None) for C in xs]
