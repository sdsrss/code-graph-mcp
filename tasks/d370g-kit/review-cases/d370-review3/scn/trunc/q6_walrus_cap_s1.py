def chain(n, walrus=False):
    s = "class K0:\n    def m(self):\n        pass\n"
    for i in range(1, n):
        s += f"class K{i}(K{i-1}):\n    pass\n"
    if walrus:
        s += f"class K{n}(K{n-1}):\n    def z(self, a=(m := len)):\n        pass\n"
    else:
        s += f"class K{n}(K{n-1}):\n    pass\n"
    return s + f"def f(x):\n    return K{n}.m(x)\n"
BEFORE = {"a.py": chain(62)}
STEPS = [{"a.py": chain(64)}, {"a.py": chain(63)}, {"a.py": chain(63, True)}, {"a.py": chain(10)}]

STEPS = STEPS[:1]
