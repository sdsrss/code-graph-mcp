from pkg.d import handler, use, Model

def m01(z):
    # inner comprehension is the outer's first iterable: runs outside the outer scope
    return [handler for handler in [use(handler) for _ in z]]

def m02(handler):
    # lambda body is a bare name at the exact body start
    return (lambda: handler), [x for x in handler]

def m02b(z):
    return [use(Model) for Model in z if Model] + [Model]

def m03(xs):
    return sorted(xs, key=lambda handler: handler)
