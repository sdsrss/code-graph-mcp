class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

import contextlib
with contextlib.nullcontext(len) as C.m: pass

def call(x):
    return C.m(x)
