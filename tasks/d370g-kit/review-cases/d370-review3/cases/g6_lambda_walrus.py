def other(self):
    return "other"

class C:
    def m(self):
        return "C.m"
    f = lambda a=(m := other): a

def call(x):
    return C.m(x)

print(call(None))
