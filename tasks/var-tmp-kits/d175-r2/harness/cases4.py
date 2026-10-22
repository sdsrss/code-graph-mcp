import runpy, os, itertools
here = os.path.dirname(os.path.abspath(__file__))
c3 = runpy.run_path(os.path.join(here, "cases3.py"))
V, tree, fill_edit, callers_edit = c3["V"], c3["tree"], c3["fill_edit"], c3["callers_edit"]
CASES = {}
pairs = list(itertools.permutations(V, 2)) + [(a, a) for a in V]
for a, b in pairs:
    step = dict(fill_edit(520, "e"))
    step.update(callers_edit("e"))
    step["pkg/m.py"] = ("# e\n" + V[b]) if a == b else V[b]
    CASES[f"b_{a}__{b}"] = {"init": tree(V[a], filler=520), "steps": [step]}
