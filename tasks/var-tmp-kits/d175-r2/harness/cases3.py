import runpy, os
c2 = runpy.run_path(os.path.join(os.path.dirname(os.path.abspath(__file__)), "cases2.py"))
V, CALLER, CALLER_IMP, CALLER_CLS, CALLER_STAR = (c2[k] for k in ("V", "CALLER", "CALLER_IMP", "CALLER_CLS", "CALLER_STAR"))

def tree(m, n=None, filler=0):
    t = {"pkg/__init__.py": "", "pkg/m.py": m, "app.py": CALLER, "app_imp.py": CALLER_IMP,
         "app_cls.py": CALLER_CLS, "app_star.py": CALLER_STAR}
    if n is not None:
        t["pkg/n.py"] = n
    for i in range(filler):
        t[f"f/{i:03d}.py"] = f"def filler_{i}():\n    return {i}\n"
    return t

def fill_edit(filler, tag):
    return {f"f/{i:03d}.py": f"# {tag}\ndef filler_{i}():\n    return {i}\n" for i in range(filler)}

def callers_edit(tag):
    return {"app.py": f"# {tag}\n" + CALLER, "app_imp.py": f"# {tag}\n" + CALLER_IMP,
            "app_cls.py": f"# {tag}\n" + CALLER_CLS, "app_star.py": f"# {tag}\n" + CALLER_STAR}

CASES = {}
# 1. several files in one run: two namesake files edited together, callers untouched
for a, b in [("fn_cy", "fn_cy"), ("cy", "fn"), ("fn", "cy"), ("cy_dy", "fn_cy"), ("fn_cy", "fn_dy")]:
    CASES[f"two_files_{a}_{b}"] = {
        "init": tree(V[a], V[b]),
        "steps": [{"pkg/m.py": "# e\n" + V[a], "pkg/n.py": "# e\n" + V[b]},
                  {"pkg/m.py": V[b], "pkg/n.py": V[a]}]}
# 2. callers in an earlier batch than m.py (>500 files in the run)
for a, b in [("cy", "fn_cy"), ("fn_cy", "fn_cy"), ("cy_dy", "fn_cy"), ("cy", "nest_cy"), ("cy", "cls_cy"),
             ("fn_cy", "cy"), ("fn_cy", "fn_dy"), ("nest_cy", "nest_cy")]:
    step = dict(fill_edit(600, "e"))
    step.update(callers_edit("e"))
    step["pkg/m.py"] = ("# e\n" + V[b]) if a == b else V[b]
    CASES[f"batches_{a}_{b}"] = {"init": tree(V[a], filler=600), "steps": [step]}
# 3. deletes
CASES["del_n_fn"] = {"init": tree(V["cy"], V["fn"]), "steps": [{"pkg/n.py": None}]}
CASES["del_n_fncy"] = {"init": tree(V["cy"], V["fn_cy"]), "steps": [{"pkg/n.py": None}]}
CASES["del_m_fncy"] = {"init": tree(V["fn_cy"], V["fn"]), "steps": [{"pkg/m.py": None}]}
CASES["del_then_readd"] = {"init": tree(V["fn_cy"]), "steps": [{"pkg/m.py": None}, {"pkg/m.py": V["fn_cy"]}]}
CASES["del_readd_other"] = {"init": tree(V["fn_cy"]), "steps": [{"pkg/m.py": None}, {"pkg/m.py": V["cy"]}, {"pkg/m.py": V["fn_cy"]}]}
# 4. renames
CASES["rename_m"] = {"init": tree(V["fn_cy"]), "steps": [{"pkg/m.py": None, "pkg/m2.py": V["fn_cy"]}]}
CASES["rename_back"] = {"init": tree(V["fn_cy"]), "steps": [{"pkg/m.py": None, "pkg/m2.py": V["fn_cy"]},
                                                          {"pkg/m2.py": None, "pkg/m.py": V["fn_cy"]}]}
CASES["move_fn_out"] = {"init": tree(V["fn_cy"]), "steps": [{"pkg/m.py": V["cy"], "pkg/n.py": V["fn"]}]}
CASES["move_fn_in"] = {"init": tree(V["cy"], V["fn"]), "steps": [{"pkg/m.py": V["fn_cy"], "pkg/n.py": None}]}
CASES["move_meth_out"] = {"init": tree(V["fn_cy"]), "steps": [{"pkg/m.py": V["fn"], "pkg/n.py": V["cy"]}]}
CASES["move_meth_in"] = {"init": tree(V["fn"], V["cy"]), "steps": [{"pkg/m.py": V["fn_cy"], "pkg/n.py": None}]}
CASES["swap_files"] = {"init": tree(V["fn"], V["cy"]), "steps": [{"pkg/m.py": V["cy"], "pkg/n.py": V["fn"]}]}
# 5. multi-step sequences moving the namesake between method/function/nested/class
seq = ["fn_cy", "cy", "fn_cy", "nest_cy", "cls_cy", "fn_cy", "fn_dy", "fn_cy", "cy_dy", "fn_cy_dy", "fn_cy"]
CASES["seq_long"] = {"init": tree(V[seq[0]]), "steps": [{"pkg/m.py": V[s] + f"# {i}\n"} for i, s in enumerate(seq[1:])]}
seq2 = ["cy", "fn_cy", "fn_cy", "cy", "cy", "fn_cy"]
CASES["seq_comment_mix"] = {"init": tree(V[seq2[0]]), "steps": [{"pkg/m.py": V[s] + f"# {i}\n"} for i, s in enumerate(seq2[1:])]}
