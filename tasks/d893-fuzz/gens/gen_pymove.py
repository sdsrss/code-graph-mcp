"""gen_review.py's states plus explicit moves and swaps of whole files (B's new trigger)."""
import importlib.util, os
_spec = importlib.util.spec_from_file_location("gen_review_base", os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "gen_review.py"))
_g = importlib.util.module_from_spec(_spec); _spec.loader.exec_module(_g)
EXTRA = ["app/moved/m.py", "pkg/n.py", "lib/k2.py", "app/sub/impl2.py", "tests/h2.py", "other2.py", "pkg/__init__.py"]
initial = _g.initial
def step(state, rng):
    st = dict(state)
    for _ in range(rng.randint(1, 3)):
        r = rng.random()
        files = sorted(st)
        if r < 0.3 and files:
            src = rng.choice(files)
            dst = rng.choice([p for p in EXTRA + _g.ALL if p != src])
            st[dst] = st.pop(src)
        elif r < 0.45 and len(files) >= 2:
            x, y = rng.sample(files, 2)
            st[x], st[y] = st[y], st[x]
        else:
            st = _g.step(st, rng)
    return st
