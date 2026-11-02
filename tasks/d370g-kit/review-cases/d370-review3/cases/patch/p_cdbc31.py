class C:
    def m(self):
        return "C.m"

*C.m, = [len]

def call(x):
    return C.m(x)
