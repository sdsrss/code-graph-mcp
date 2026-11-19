from pkg import a
helper, deco = a.helper, a.deco
Model2 = Model = a.Model
(other_helper) = a.helper

def use():
    register(helper)
    return Model

@deco
def decorated():
    pass

def typed(x: Model) -> Model:
    return x

callbacks = [helper]
def register(f):
    return f
