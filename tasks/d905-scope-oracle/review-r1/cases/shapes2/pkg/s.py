from pkg.d import handler, Model, register
import pkg.d as d

class A:
    def register(f): return f            # method named like the import
    @register                            # S1: class-bound decorator
    def m(self): pass
    def gen(): return [1]
    xs = [y for y in gen()]              # S2: first iterable sees class scope
    cb = [handler for _ in range(2)]     # element: module handler
    f = lambda cb=handler: cb            # S4: lambda default, class scope
    def n(self, x: "Model") -> "Model":  # S13: string annotation
        return x

def sm(cmd):
    match cmd:
        case Model():                    # class pattern
            return handler
        case {"k": handler2}:
            return handler2

try:
    from pkg.d import parse
except ImportError:
    def parse(x): return x
p = parse                                # S7

def g():
    open = d.open                        # local alias of a project def named like a builtin
    def inner():
        return open                      # reads g's open (== d.open)
    return inner

def h():
    return [open for open in [d.open]]   # comprehension var

def k(x=open):                           # builtin read in default
    return x

globals()["helper2"] = d.handler
def uses_helper2():
    return helper2
