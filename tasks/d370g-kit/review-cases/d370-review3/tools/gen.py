import sys
kind, k = sys.argv[1], int(sys.argv[2])
if kind == "dup":      # class C(A, A, ..., A): duplicate file-class base, Python refuses (duplicate base)
    print("class A:\n    pass\n\nclass C(" + ",".join(["A"] * k) + "):\n    pass\n\ndef f(x):\n    return C.m(x)")
elif kind == "leaves": # distinct unknown bases
    print("class C(" + ",".join(f"x{i}" for i in range(k)) + "):\n    pass\n\ndef f(x):\n    return C.m(x)")
elif kind == "body":   # k statements in one class body
    print("class C:\n" + "".join(f"    a{i}=1\n" for i in range(k)) + "    def m(self):\n        pass\n\ndef f(x):\n    return C.m(x)")
elif kind == "dupnocall":  # same as dup, but no C.m() call
    print("class A:\n    pass\n\nclass C(" + ",".join(["A"] * k) + "):\n    pass\n")
elif kind == "overlap":  # K0 defines m; Ki(K(i-1), ..., K0); call K(k-1).m
    out = ["class K0:\n    def m(self):\n        pass\n"]
    for i in range(1, k):
        out.append(f"class K{i}(" + ", ".join(f"K{j}" for j in range(i - 1, -1, -1)) + "):\n    pass\n")
    out.append(f"def f(x):\n    return K{k-1}.m(x)\n")
    print("\n".join(out))
elif kind == "chain":    # K0 defines m; Ki(K(i-1)); call K(k-1).m
    out = ["class K0:\n    def m(self):\n        pass\n"] + [f"class K{i}(K{i-1}):\n    pass\n" for i in range(1, k)]
    out.append(f"def f(x):\n    return K{k-1}.m(x)\n")
    print("\n".join(out))
elif kind == "qshared":  # Q chain of 16, 47 B_j(Q0), then X_n(B1..B47) until ~1 MiB; valid Python, MRO 64
    out = ["class Q15:\n    def m(self):\n        pass\n"] + [f"class Q{i}(Q{i+1}):\n    pass\n" for i in range(14, -1, -1)]
    out += [f"class B{j}(Q0):\n    pass\n" for j in range(1, 48)]
    bases = ",".join(f"B{j}" for j in range(1, 48))
    size = sum(len(s) + 1 for s in out); n = 0
    while size < k:
        s = f"class X{n}({bases}):pass\n"; out.append(s); size += len(s) + 1; n += 1
    out.append(f"def f(x):\n    return X0.m(x)\n")
    print("\n".join(out))
elif kind == "qsharednocall":
    out = ["class Q15:\n    def m(self):\n        pass\n"] + [f"class Q{i}(Q{i+1}):\n    pass\n" for i in range(14, -1, -1)]
    out += [f"class B{j}(Q0):\n    pass\n" for j in range(1, 48)]
    bases = ",".join(f"B{j}" for j in range(1, 48))
    size = sum(len(s) + 1 for s in out); n = 0
    while size < k:
        s = f"class X{n}({bases}):pass\n"; out.append(s); size += len(s) + 1; n += 1
    out.append(f"def f(x):\n    return x.m(x)\n")
    print("\n".join(out))
elif kind == "nest":   # one class C, then x = [[[...[C, C, ..., C]...]]] with depth d = k and k occurrences
    d = k
    print("class C:\n    def m(self):\n        pass\n\nx = " + "[" * d + ",".join(["C"] * k) + "]" * d + "\n\ndef f(y):\n    return C.m(y)")
elif kind == "big3000":  # chain K0..K61 (K0 defines m), then k classes X_n(K61, ..., K0), one call
    out = ["class K0:\n    def m(self):\n        pass\n"] + [f"class K{i}(K{i-1}):\n    pass\n" for i in range(1, 62)]
    bases = ", ".join(f"K{j}" for j in range(61, -1, -1))
    out += [f"class X{n}({bases}):\n    pass\n" for n in range(k)]
    out.append(f"def f(x):\n    return X0.m(x)\n")
    print("\n".join(out))
elif kind == "big3000calls":  # as big3000 but one function per class calling X_n.m
    out = ["class K0:\n    def m(self):\n        pass\n"] + [f"class K{i}(K{i-1}):\n    pass\n" for i in range(1, 62)]
    bases = ",".join(f"K{j}" for j in range(61, -1, -1))
    out += [f"class X{n}({bases}):\n    def g(self):\n        return X{n}.m(self)\n" for n in range(k)]
    print("\n".join(out))
