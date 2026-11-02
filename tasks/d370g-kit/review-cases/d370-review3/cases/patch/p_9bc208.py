class C:
    def m(self):
        return "C.m"

y, *C.m = [1, len]

def call(x):
    return C.m(x)
