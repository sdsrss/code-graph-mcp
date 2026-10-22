import runpy, os, itertools
here = os.path.dirname(os.path.abspath(__file__))
V = runpy.run_path(os.path.join(here, "cases2.py"))["V"]
REL = "from .m import Y\n\ndef rel_call():\n    return Y()\n\ndef rel_ref():\n    return Y\n"
AS = "from pkg.m import Y as Z\n\ndef as_call():\n    return Z()\n\ndef as_ref():\n    return Z\n"
RX = "from pkg import Y\n\ndef rx_call():\n    return Y()\n\ndef rx_ref():\n    return Y\n"
RXMOD = "import pkg\n\ndef rxm_call():\n    return pkg.Y()\n"
def tree(m):
    return {"pkg/__init__.py": "from .m import Y\n", "pkg/m.py": m, "pkg/b.py": REL,
            "app_as.py": AS, "app_rx.py": RX, "app_rxm.py": RXMOD}
CASES = {}
for a, b in list(itertools.permutations(V, 2)):
    CASES[f"{a}__{b}"] = {"init": tree(V[a]), "steps": [{"pkg/m.py": V[b]}]}
for a in V:
    CASES[f"{a}__comment"] = {"init": tree(V[a]), "steps": [{"pkg/m.py": "# edit\n" + V[a]}]}
