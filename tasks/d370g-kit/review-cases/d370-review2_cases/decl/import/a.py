class B:
    def m(self): pass
class C(B):
    def m(self): pass
import C
C.m(None)
