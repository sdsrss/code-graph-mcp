class A:
    def m(self):
        return "A"

class S:
    __slots__ = ("m",)

class D(S, A):
    pass

def call(x):
    return D.m(x)

print([k.__name__ for k in D.__mro__], type(D.m).__name__)
try:
    call(None)
except TypeError as e:
    print("TypeError:", e)
