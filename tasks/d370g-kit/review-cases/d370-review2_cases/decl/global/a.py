class B:
    def m(self): pass
class C(B):
    def m(self): pass
def f():
    global C
    C.m(None)
