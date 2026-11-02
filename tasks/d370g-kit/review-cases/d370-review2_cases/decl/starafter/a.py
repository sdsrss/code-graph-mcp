class B:
    def m(self): pass
class C(B):
    def m(self): pass
from x import *
C.m(None)
