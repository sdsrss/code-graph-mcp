class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

y = C.m = len

def call(x):
    return C.m(x)
