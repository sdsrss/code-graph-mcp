from pkg import a
Model = Model2 = a.Model          # chained, alias first
deco: object = a.deco             # annotated alias (kept)
(helper, Base) = (a.helper, a.Base)   # parenthesized tuple alias

def use_model():
    return Model

@deco
def decorated():
    pass

def use_helper():
    return helper

def typed(x: Base) -> None:
    pass
