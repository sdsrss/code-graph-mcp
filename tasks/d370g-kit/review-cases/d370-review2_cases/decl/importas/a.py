class B:
    def m(self): pass
class C(B):
    def m(self): pass
import a as C
C.m(None)
