# S06/S08: global rebinding
def h():
    return "module h"
def f():
    global h
    def h():
        return "f's nested h"
f()
def g():
    return h
print("S06", g()())

class Row:
    tag = "module Row"
def setup():
    global Row
    class Row:
        tag = "setup's Row"
setup()
def use():
    return Row()
print("S08", use().tag)

# S10 nonlocal
def outer():
    def h():
        return "outer h"
    def inner():
        nonlocal h
        def h():
            return "inner h"
    inner()
    return h
print("S10", outer()())

# S11 class body LOAD_NAME skips enclosing fn
def helper():
    return "module helper"
def outer2():
    def helper():
        return "outer2 helper"
    class C:
        x = staticmethod(helper)
        def helper(self):
            pass
    return C
print("S11", outer2().x())

# S12 free name in class body
def outer3():
    def helper():
        return "outer3 helper"
    class C:
        x = staticmethod(helper)
    return C
print("S12", outer3().x())

# S15 header call to nested class
class Row2:
    tag = "module Row2"
def f15(x=Row2()):
    class Row2:
        tag = "nested Row2"
    return x
print("S15", f15().tag)
