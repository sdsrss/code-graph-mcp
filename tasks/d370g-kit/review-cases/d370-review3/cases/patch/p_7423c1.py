class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

C.m, y = len, 1

def call(x):
    return C.m(x)
