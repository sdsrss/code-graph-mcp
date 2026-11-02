from collections import OrderedDict as X

class C(X, X):
    def m(self):
        return "C"

def call(x):
    return C.m(x)
