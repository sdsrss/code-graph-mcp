from pkg.d import Model, handler

def gen[T: Model](x: T) -> T:          # bound reads Model at module level
    return x

class Box[T]:
    def get(self) -> T: ...
    def put(self, x: Model) -> None: ...

type Alias = Model

def m(cmd):
    match cmd:
        case Model():
            return use(handler)
        case [first, *rest]:
            return use(first)

def outer():
    handler2 = 1
    def mid():
        global handler
        def inner():
            return handler          # mid's global sends it to module: pkg.d.handler
        return inner
    def mid2():
        nonlocal handler2
        return handler2
    return mid, mid2

def use(x): return x
