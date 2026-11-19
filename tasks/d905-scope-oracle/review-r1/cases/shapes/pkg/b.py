from pkg import a
from pkg.a import handler as _handler
handler = _handler                      # B: rename back

def outer_alias():                      # A: enclosing function alias
    helper = a.helper
    def inner():
        return helper                   # reads outer's helper (== a.helper)
    def inner2():
        cb = helper
        helper()                        # call through enclosing alias
    return inner

def use_renamed():
    return handler                      # B

def nested_def_vs_param(deco):          # param, nested def rebinding it
    def deco(f): return f
    def inner():
        return deco                     # Python: the nested def deco
    return inner

class K:
    Inner = a.Inner                     # class alias, read in class body
    @staticmethod
    def m(x: Inner) -> Inner:           # annotation in class body sees class names
        return x
    xs = [i for i in items()]           # items is a module-level name? no -> unbound

def walrus_in_comp(xs):
    ys = [(Model := x) for x in xs]
    return Model
