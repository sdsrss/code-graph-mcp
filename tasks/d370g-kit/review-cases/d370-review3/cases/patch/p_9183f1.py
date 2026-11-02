class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

del C.n, (C).m

def call(x):
    return C.m(x)
