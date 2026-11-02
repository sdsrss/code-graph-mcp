class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

C.m = C.n = len

def call(x):
    return C.m(x)
