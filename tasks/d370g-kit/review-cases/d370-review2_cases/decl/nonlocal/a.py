class B:
    def m(self): pass
class C(B):
    def m(self): pass
def f():
    def g():
        nonlocal C
        C.m(None)
