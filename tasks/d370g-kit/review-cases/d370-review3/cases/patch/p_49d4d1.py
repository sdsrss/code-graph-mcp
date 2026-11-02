class C:
    def m(self):
        return "C.m"
    def n(self):
        pass

C.m: object = len

def call(x):
    return C.m(x)
