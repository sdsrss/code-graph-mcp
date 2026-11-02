class B:
    def m(self): pass
class C(B):
    def m(self): pass
try:
    pass
except E as C:
    C.m(None)
