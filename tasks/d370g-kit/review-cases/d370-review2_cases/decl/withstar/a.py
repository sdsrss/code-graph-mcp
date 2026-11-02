class B:
    def m(self): pass
class C(B):
    def m(self): pass
with f() as (a, *C):
    C.m(None)
