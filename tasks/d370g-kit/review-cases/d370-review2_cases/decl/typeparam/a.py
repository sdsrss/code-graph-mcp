class B:
    def m(self): pass
class C(B):
    def m(self): pass
def f[C](x):
    C.m(x)
